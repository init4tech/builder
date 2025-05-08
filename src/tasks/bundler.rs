//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::tasks::oauth::SharedToken;
use oauth2::TokenResponse;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use signet_bundle::SignetEthBundle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{Instrument, debug, trace, warn};

pub use crate::config::BuilderConfig;

/// Holds a bundle from the cache with a unique ID and a Zenith bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// Cache identifier for the bundle.
    pub id: String,
    /// The corresponding Signet bundle.
    pub bundle: SignetEthBundle,
}

/// Response from the tx-pool containing a list of bundles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxPoolBundleResponse {
    /// Bundle responses are available on the bundles property.
    pub bundles: Vec<Bundle>,
}

/// The BundlePoller polls the tx-pool for bundles.
#[derive(Debug)]
pub struct BundlePoller {
    /// The builder configuration values.
    pub config: BuilderConfig,
    /// Authentication module that periodically fetches and stores auth tokens.
    pub token: SharedToken,
    /// Holds a Reqwest client
    pub client: Client,
    /// Defines the interval at which the bundler polls the tx-pool for bundles.
    pub poll_interval_ms: u64,
}

/// Implements a poller for the block builder to pull bundles from the tx-pool.
impl BundlePoller {
    /// Creates a new BundlePoller from the provided builder config.
    pub fn new(config: &BuilderConfig, token: SharedToken) -> Self {
        Self { config: config.clone(), token, client: Client::new(), poll_interval_ms: 1000 }
    }

    /// Creates a new BundlePoller from the provided builder config and with the specified poll interval in ms.
    pub fn new_with_poll_interval_ms(
        config: &BuilderConfig,
        token: SharedToken,
        poll_interval_ms: u64,
    ) -> Self {
        Self { config: config.clone(), token, client: Client::new(), poll_interval_ms }
    }

    /// Fetches bundles from the transaction cache and returns them.
    pub async fn check_bundle_cache(&mut self) -> eyre::Result<Vec<Bundle>> {
        let bundle_url: Url = Url::parse(&self.config.tx_pool_url)?.join("bundles")?;
        let Some(token) = self.token.read() else {
            warn!("No token available, skipping bundle fetch");
            return Ok(vec![]);
        };

        let result = self
            .client
            .get(bundle_url)
            .bearer_auth(token.access_token().secret())
            .send()
            .await?
            .error_for_status()?;

        let body = result.bytes().await?;
        let resp: TxPoolBundleResponse = serde_json::from_slice(&body)?;

        Ok(resp.bundles)
    }

    async fn task_future(mut self, outbound: UnboundedSender<Bundle>) {
        loop {
            let span = tracing::debug_span!("BundlePoller::loop", url = %self.config.tx_pool_url);

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

            match self.check_bundle_cache().instrument(span.clone()).await {
                Ok(bundles) => {
                    tracing::debug!(count = ?bundles.len(), "found bundles");
                    for bundle in bundles.into_iter() {
                        if let Err(err) = outbound.send(bundle) {
                            tracing::error!(err = ?err, "Failed to send bundle - channel is dropped");
                        }
                    }
                }
                // If fetching was an error, we log and continue. We expect
                // these to be transient network issues.
                Err(e) => {
                    debug!(error = %e, "Error fetching bundles");
                }
            }
            time::sleep(time::Duration::from_millis(self.poll_interval_ms)).await;
        }
    }

    /// Spawns a task that sends bundles it finds to its channel sender.
    pub fn spawn(self) -> (UnboundedReceiver<Bundle>, JoinHandle<()>) {
        let (outbound, inbound) = unbounded_channel();

        let jh = tokio::spawn(self.task_future(outbound));

        (inbound, jh)
    }
}
