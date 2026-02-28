//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::config::BuilderConfig;
use init4_bin_base::{
    deps::metrics::{counter, histogram},
    perms::tx_cache::{BuilderTxCache, BuilderTxCacheError},
};
use signet_tx_cache::{TxCacheError, types::CachedBundle};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
    time::{self, Duration},
};
use tracing::{Instrument, error, trace, trace_span};

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
                histogram!("signet.builder.cache.bundles_fetched")
                    .record(bundles.bundles.len() as f64);
                trace!(count = ?bundles.bundles.len(), "found bundles");
                Ok(bundles.bundles)
            }
            Err(err) => {
                if matches!(&err, BuilderTxCacheError::TxCache(TxCacheError::NotOurSlot)) {
                    trace!("Not our slot to fetch bundles");
                } else {
                    counter!("signet.builder.cache.bundle_poll_errors").increment(1);
                    error!(?err, "Failed to fetch bundles from tx-cache");
                }
                Err(err)
            }
        }
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
                    if let Err(err) = outbound.send(bundle) {
                        span_debug!(span, ?err, "Failed to send bundle - channel is dropped");
                        break;
                    }
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
