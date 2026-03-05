//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::config::BuilderConfig;
use init4_bin_base::{
    deps::metrics::{counter, histogram},
    perms::tx_cache::{BuilderTxCache, BuilderTxCacheError},
};
use signet_tx_cache::{
    TxCacheError,
    types::{BundleKey, CachedBundle},
};
use alloy::{
    consensus::{Transaction, transaction::SignerRecoverable},
    eips::Decodable2718,
    primitives::Bytes,
    providers::Provider,
    rlp::Buf,
};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
    time::{self, Duration},
};
use tracing::{Instrument, debug, debug_span, warn, trace, trace_span};

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
    /// Bundles with stale host transaction nonces are dropped to prevent them from
    /// entering the SimCache, failing simulation, and being re-ingested on the next poll.
    fn spawn_check_bundle_nonces(bundle: CachedBundle, outbound: UnboundedSender<CachedBundle>) {
        tokio::spawn(async move {
            let span = debug_span!("check_bundle_nonces", bundle_id = %bundle.id);

            // If no host transactions, forward directly
            if bundle.bundle.host_txs.is_empty() {
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

            // Check each host transaction's nonce
            for (idx, host_tx_bytes) in bundle.bundle.host_txs.iter().enumerate() {
                let host_tx = match decode_tx(host_tx_bytes) {
                    Some(tx) => tx,
                    None => {
                        span_debug!(
                            span,
                            idx,
                            "Failed to decode host transaction, dropping bundle"
                        );
                        return;
                    }
                };

                let sender = match host_tx.recover_signer() {
                    Ok(s) => s,
                    Err(_) => {
                        span_debug!(
                            span,
                            idx,
                            "Failed to recover sender from host tx, dropping bundle"
                        );
                        return;
                    }
                };

                let tx_count = match host_provider.get_transaction_count(sender).await {
                    Ok(count) => count,
                    Err(_) => {
                        span_debug!(span, idx, %sender, "Failed to fetch nonce for sender, dropping bundle");
                        return;
                    }
                };

                if host_tx.nonce() < tx_count {
                    debug!(
                        parent: &span,
                        %sender,
                        tx_nonce = %host_tx.nonce(),
                        host_nonce = %tx_count,
                        idx,
                        "Dropping bundle with stale host tx nonce"
                    );
                    return;
                }
            }

            // All host txs have valid nonces, forward the bundle
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

/// Decodes a transaction from RLP-encoded bytes.
fn decode_tx(bytes: &Bytes) -> Option<alloy::consensus::TxEnvelope> {
    alloy::consensus::TxEnvelope::decode_2718(&mut bytes.chunk()).ok()
}
