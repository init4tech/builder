//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::config::BuilderConfig;
use alloy::{consensus::TxEnvelope, eips::eip2718::Decodable2718};
use init4_bin_base::perms::SharedToken;
use reqwest::{Client, Url};
use signet_tx_cache::types::{TxCacheBundle, TxCacheBundlesResponse};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
    time::{self, Duration},
};
use tracing::{Instrument, debug, debug_span, error, trace};

/// Poll interval for the bundle poller in milliseconds.
const POLL_INTERVAL_MS: u64 = 1000;

/// The BundlePoller polls the tx-pool for bundles.
#[derive(Debug)]
pub struct BundlePoller {
    /// The builder configuration values.
    config: &'static BuilderConfig,
    /// Authentication module that periodically fetches and stores auth tokens.
    token: SharedToken,
    /// Holds a Reqwest client
    client: Client,
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
        let token = config.oauth_token();
        Self { config, token, client: Client::new(), poll_interval_ms }
    }

    /// Fetches bundles from the transaction cache and returns them.
    pub async fn check_bundle_cache(&mut self) -> eyre::Result<Vec<TxCacheBundle>> {
        let bundle_url: Url = self.config.tx_pool_url.join("bundles")?;
        let token =
            self.token.secret().await.map_err(|e| eyre::eyre!("Failed to read token: {e}"))?;

        self.client
            .get(bundle_url)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .map(|resp: TxCacheBundlesResponse| resp.bundles)
            .map_err(Into::into)
    }

    /// Returns the poll duration as a [`Duration`].
    const fn poll_duration(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    async fn task_future(mut self, outbound: UnboundedSender<TxCacheBundle>) {
        loop {
            let span = debug_span!("BundlePoller::loop", url = %self.config.tx_pool_url);

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

            if let Ok(bundles) = self
                .check_bundle_cache()
                .instrument(span.clone())
                .await
                .inspect_err(|err| debug!(%err, "Error fetching bundles"))
            {
                let _guard = span.entered();
                debug!(count = ?bundles.len(), "found bundles");
                for bundle in bundles.into_iter() {

                    decode_and_log_host_transactions(&bundle);

                    if let Err(err) = outbound.send(bundle) {
                        error!(err = ?err, "Failed to send bundle - channel is dropped");
                        break;
                    }
                }
            }

            time::sleep(self.poll_duration()).await;
        }
    }

    /// Spawns a task that sends bundles it finds to its channel sender.
    pub fn spawn(self) -> (UnboundedReceiver<TxCacheBundle>, JoinHandle<()>) {
        let (outbound, inbound) = unbounded_channel();

        let jh = tokio::spawn(self.task_future(outbound));

        (inbound, jh)
    }
}

fn decode_and_log_host_transactions(bundle: &TxCacheBundle) {
    for (idx, bz) in bundle.bundle().host_txs.iter().enumerate() {
        // Best-effort decode so we can surface the transaction type in logs.
        let mut raw = bz.as_ref();
        match TxEnvelope::decode_2718(&mut raw) {
            Ok(envelope) => {
                trace!(
                    bundle_id = %bundle.id(),
                    host_tx_idx = idx,
                    tx_type = ?envelope.tx_type(),
                    leftover_bytes = raw.len(),
                    "decoded host transaction as eip-2718"
                );
            }
            Err(err) => {
                debug!(
                    bundle_id = %bundle.id(),
                    host_tx_idx = idx,
                    err = %err,
                    "failed to decode host transaction as eip-2718"
                );
            }
        }
    }
}
