//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::config::BuilderConfig;
use init4_bin_base::{
    deps::metrics::{counter, histogram},
    perms::tx_cache::{BuilderTxCache, BuilderTxCacheError},
};
use signet_sim::{ProviderStateSource, SimItemValidity, check_bundle_tx_list};
use signet_tx_cache::{
    TxCacheError,
    types::{BundleKey, CachedBundle},
};
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

    /// Spawns a tokio task to check the validity of all host transactions in a
    /// bundle before sending it to the cache task via the outbound channel.
    ///
    /// Uses [`check_bundle_tx_list`] from `signet-sim` to validate host tx nonces
    /// and balance against the host chain. Drops bundles that are not currently valid.
    fn spawn_check_bundle_nonces(bundle: CachedBundle, outbound: UnboundedSender<CachedBundle>) {
        let span = debug_span!("check_bundle_nonces", bundle_id = %bundle.id);
        tokio::spawn(async move {
            let recovered = match bundle.bundle.try_to_recovered() {
                Ok(recovered) => recovered,
                Err(error) => {
                    span_debug!(span, ?error, "Failed to recover bundle, dropping");
                    return;
                }
            };

            if recovered.host_txs().is_empty() {
                if outbound.send(bundle).is_err() {
                    span_debug!(span, "Outbound channel closed");
                }
                return;
            }

            let Ok(host_provider) =
                crate::config().connect_host_provider().instrument(span.clone()).await
            else {
                span_debug!(span, "Failed to connect to host provider, dropping bundle");
                return;
            };

            let source = ProviderStateSource(host_provider);
            match check_bundle_tx_list(recovered.host_tx_reqs(), &source).await {
                Ok(SimItemValidity::Now) | Ok(SimItemValidity::Future) => {
                    if outbound.send(bundle).is_err() {
                        span_debug!(span, "Outbound channel closed");
                    }
                }
                Ok(SimItemValidity::Never) => {
                    span_debug!(span, "Dropping bundle: host txs will never be valid");
                }
                Err(error) => {
                    span_debug!(span, %error, "Failed to check bundle validity, dropping");
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
