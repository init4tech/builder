//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::config::BuilderConfig;
use alloy::providers::Provider;
use futures_util::future::try_join_all;
use init4_bin_base::{
    deps::metrics::{counter, histogram},
    perms::tx_cache::{BuilderTxCache, BuilderTxCacheError},
};
use signet_tx_cache::{
    TxCacheError,
    types::{BundleKey, CachedBundle},
};
use std::collections::{BTreeMap, BTreeSet};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
    time::{self, Duration},
};
use tracing::{Instrument, debug_span, trace, trace_span, warn};

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

    /// Fetches all bundles from the tx-cache, paginating through all available pages.
    pub async fn check_bundle_cache(&self) -> Result<Vec<CachedBundle>, BuilderTxCacheError> {
        let mut all_bundles = Vec::new();
        let mut cursor: Option<BundleKey> = None;

        loop {
            let resp = match self.tx_cache.get_bundles(cursor).await {
                Ok(resp) => resp,
                Err(error) => {
                    if matches!(&error, BuilderTxCacheError::TxCache(TxCacheError::NotOurSlot)) {
                        trace!("Not our slot to fetch bundles");
                    } else {
                        counter!("signet.builder.cache.bundle_poll_errors").increment(1);
                        warn!(%error, "Failed to fetch bundles from tx-cache");
                    }
                    return Err(error);
                }
            };

            let (bundle_list, next_cursor) = resp.into_parts();
            all_bundles.extend(bundle_list.bundles);

            let Some(next) = next_cursor else { break };
            cursor = Some(next);
        }

        trace!(count = all_bundles.len(), "fetched all bundles from tx-cache");
        histogram!("signet.builder.cache.bundles_fetched").record(all_bundles.len() as f64);
        Ok(all_bundles)
    }

    /// Spawns a tokio task to check the nonces of all host transactions in a bundle
    /// before sending it to the cache task via the outbound channel.
    ///
    /// Fetches on-chain nonces concurrently for each unique signer, then validates
    /// sequentially with a local nonce cache — mirroring the SDK's
    /// `check_bundle_tx_list` pattern. Drops bundles where any host tx has a stale
    /// or future nonce.
    fn spawn_check_bundle_nonces(bundle: CachedBundle, outbound: UnboundedSender<CachedBundle>) {
        let span = debug_span!("check_bundle_nonces", bundle_id = %bundle.id);
        tokio::spawn(async move {
            // Recover the bundle to get typed host tx requirements instead of
            // manually decoding and recovering signers.
            let recovered = match bundle.bundle.try_to_recovered() {
                Ok(r) => r,
                Err(error) => {
                    span_debug!(span, ?error, "Failed to recover bundle, dropping");
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
            let reqs: Vec<_> = recovered.host_tx_reqs().collect();

            // Fetch on-chain nonces concurrently for each unique signer
            let unique_signers: BTreeSet<_> = reqs.iter().map(|req| req.signer).collect();
            let nonce_fetches = unique_signers.into_iter().map(|signer| {
                let host_provider = &host_provider;
                let span = &span;
                async move {
                    host_provider
                        .get_transaction_count(signer)
                        .await
                        .map(|nonce| (signer, nonce))
                        .inspect_err(|error| {
                            span_debug!(
                                span,
                                ?error,
                                sender = %signer,
                                "Failed to fetch nonce for sender, dropping bundle"
                            );
                        })
                }
            });

            let Ok(fetched) = try_join_all(nonce_fetches).await else {
                return;
            };
            let mut nonce_cache: BTreeMap<_, _> = fetched.into_iter().collect();

            // Validate sequentially, checking exact nonce match and incrementing for
            // same-signer sequential txs (mirroring check_bundle_tx_list in signet-sim).
            for (idx, req) in reqs.iter().enumerate() {
                let expected = nonce_cache.get(&req.signer).copied().expect("nonce must be cached");

                if req.nonce != expected {
                    span_debug!(
                        span,
                        sender = %req.signer,
                        tx_nonce = req.nonce,
                        expected_nonce = expected,
                        idx,
                        "Dropping bundle: host tx nonce mismatch"
                    );
                    return;
                }

                nonce_cache.entry(req.signer).and_modify(|nonce| *nonce += 1);
            }

            if outbound.send(bundle).is_err() {
                span_debug!(span, "Outbound channel closed, stopping nonce check task");
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

            counter!("signet.builder.cache.bundle_poll_count").increment(1);
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
