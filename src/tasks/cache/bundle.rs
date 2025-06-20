//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::config::BuilderConfig;
use eyre::bail;
use init4_bin_base::{
    deps::tracing::{Instrument, debug, debug_span, error, trace, warn},
    perms::SharedToken,
};
use oauth2::TokenResponse;
use reqwest::{Client, Url};
use signet_tx_cache::types::{TxCacheBundle, TxCacheBundlesResponse};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
    time::{self, Duration},
};

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
    pub async fn check_bundle_cache(&mut self) -> eyre::Result<Vec<TxCacheBundle>> {
        let bundle_url: Url = Url::parse(&self.config.tx_pool_url)?.join("bundles")?;
        let Some(token) = self.token.read() else {
            bail!("No token available, skipping bundle fetch");
        };

        self.client
            .get(bundle_url)
            .bearer_auth(token.access_token().secret())
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
