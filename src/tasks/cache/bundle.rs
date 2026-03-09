//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::config::BuilderConfig;
use alloy::providers::Provider;
use futures_util::{TryStreamExt, stream};
use init4_bin_base::perms::tx_cache::{BuilderTxCache, BuilderTxCacheError};
use signet_tx_cache::{TxCacheError, types::CachedBundle};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
    time::{self, Duration},
};
use tracing::{Instrument, debug, debug_span, error, trace, trace_span};

/// Poll interval for the bundle poller in milliseconds.
const POLL_INTERVAL_MS: u64 = 1000;

/// The BundlePoller polls the tx-pool for bundles.
#[derive(Debug)]
pub struct BundlePoller {
    /// The builder configuration values.
    config: &'static BuilderConfig,

    /// Client for the tx cache.
    tx_cache: BuilderTxCache,

    /// Defines the interval at which the bundler polls the tx-pool for bundles.
    poll_interval_ms: u64,
}

impl Default for BundlePoller {
    fn default() -> Self {
        Self::new()
    }
}

/// Implements a poller for the block builder to pull bundles from the tx-pool.
impl BundlePoller {
    /// Creates a new BundlePoller from the provided builder config.
    pub fn new() -> Self {
        Self::new_with_poll_interval_ms(POLL_INTERVAL_MS)
    }

    /// Creates a new BundlePoller from the provided builder config and with the specified poll interval in ms.
    pub fn new_with_poll_interval_ms(poll_interval_ms: u64) -> Self {
        let config = crate::config();
        let tx_cache = BuilderTxCache::new(config.tx_pool_url.clone(), config.oauth_token());
        Self { config, tx_cache, poll_interval_ms }
    }

    /// Returns the poll duration as a [`Duration`].
    const fn poll_duration(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    /// Checks the bundle cache for new bundles.
    pub async fn check_bundle_cache(&self) -> Result<Vec<CachedBundle>, BuilderTxCacheError> {
        let res = self.tx_cache.get_bundles(None).await;

        match res {
            Ok(resp) => {
                let bundles = resp.into_inner();
                trace!(count = ?bundles.bundles.len(), "found bundles");
                Ok(bundles.bundles)
            }
            Err(err) => {
                if matches!(&err, BuilderTxCacheError::TxCache(TxCacheError::NotOurSlot)) {
                    trace!("Not our slot to fetch bundles");
                } else {
                    error!(?err, "Failed to fetch bundles from tx-cache");
                }
                Err(err)
            }
        }
    }

    /// Spawns a tokio task to check the nonces of all host transactions in a bundle
    /// before sending it to the cache task via the outbound channel.
    ///
    /// Uses the bundle's `host_tx_reqs()` to extract signer/nonce requirements
    /// (reusing the existing validity check pattern from `signet-sim`), then checks
    /// all host tx nonces concurrently via [`FuturesUnordered`], cancelling early
    /// on the first stale or failed nonce.
    ///
    /// [`FuturesUnordered`]: futures_util::stream::FuturesUnordered
    fn spawn_check_bundle_nonces(bundle: CachedBundle, outbound: UnboundedSender<CachedBundle>) {
        tokio::spawn(async move {
            let span = debug_span!("check_bundle_nonces", bundle_id = %bundle.id);

            // Recover the bundle to get typed host tx requirements instead of
            // manually decoding and recovering signers.
            let recovered = match bundle.bundle.try_to_recovered() {
                Ok(r) => r,
                Err(e) => {
                    span_debug!(span, ?e, "Failed to recover bundle, dropping");
                    return;
                }
            };

            // If no host transactions, forward directly
            if recovered.host_txs().is_empty() {
                if outbound.send(bundle).is_err() {
                    span_debug!(span, "Outbound channel closed, stopping nonce check task");
                }
                return;
            }

            let Ok(host_provider) =
                crate::config().connect_host_provider().instrument(span.clone()).await
            else {
                span_debug!(span, "Failed to connect to host provider, stopping nonce check task");
                return;
            };

            // Collect host tx requirements (signer + nonce) from the recovered bundle
            let reqs: Vec<_> = recovered.host_tx_reqs().enumerate().collect();

            // Check all host tx nonces concurrently, cancelling on first failure.
            let result = stream::iter(reqs)
                .map(Ok)
                .try_for_each_concurrent(None, |(idx, req)| {
                    let host_provider = &host_provider;
                    let span = &span;
                    async move {
                        let tx_count = host_provider
                            .get_transaction_count(req.signer)
                            .await
                            .map_err(|_| {
                                span_debug!(
                                    span,
                                    idx,
                                    sender = %req.signer,
                                    "Failed to fetch nonce for sender, dropping bundle"
                                );
                            })?;

                        if req.nonce < tx_count {
                            debug!(
                                parent: span,
                                sender = %req.signer,
                                tx_nonce = %req.nonce,
                                host_nonce = %tx_count,
                                idx,
                                "Dropping bundle with stale host tx nonce"
                            );
                            return Err(());
                        }

                        Ok(())
                    }
                })
                .await;

            // All host txs have valid nonces, forward the bundle
            if result.is_ok() {
                if outbound.send(bundle).is_err() {
                    span_debug!(span, "Outbound channel closed, stopping nonce check task");
                }
            }
        });
    }

    async fn task_future(self, outbound: UnboundedSender<CachedBundle>) {
        loop {
            let span = trace_span!("BundlePoller::loop", url = %self.config.tx_pool_url);

            // Enter the span for the next check.
            let _guard = span.enter();

            // Check this here to avoid making the web request if we know
            // we don't need the results.
            if outbound.is_closed() {
                trace!("No receivers left, shutting down");
                break;
            }
            // exit the span after the check.
            drop(_guard);

            if let Ok(bundles) = self.check_bundle_cache().instrument(span.clone()).await {
                for bundle in bundles.into_iter() {
                    Self::spawn_check_bundle_nonces(bundle, outbound.clone());
                }
            }

            time::sleep(self.poll_duration()).await;
        }
    }

    /// Spawns a task that sends bundles it finds to its channel sender.
    pub fn spawn(self) -> (UnboundedReceiver<CachedBundle>, JoinHandle<()>) {
        let (outbound, inbound) = unbounded_channel();

        let jh = tokio::spawn(self.task_future(outbound));

        (inbound, jh)
    }
}
