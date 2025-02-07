//! Bundler service responsible for managing bundles.
use super::oauth::Authenticator;

pub use crate::config::BuilderConfig;

use oauth2::TokenResponse;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::task::JoinHandle;
use zenith_types::ZenithEthBundle;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub id: String,
    pub bundle: ZenithEthBundle,
}

/// Response from the tx-pool containing a list of bundles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxPoolBundleResponse {
    pub bundles: Vec<Bundle>,
}

/// The BundlePoller polls the tx-pool for bundles and manages the seen bundles.
pub struct BundlePoller {
    pub config: BuilderConfig,
    pub authenticator: Authenticator,
}

/// Implements a poller for the block builder to pull bundles from the tx cache.
impl BundlePoller {
    /// Creates a new BundlePoller from the provided builder config.
    pub fn new(config: &BuilderConfig, authenticator: Authenticator) -> Self {
        Self { config: config.clone(), authenticator }
    }

    /// Fetches bundles from the transaction cache and returns the (oldest? random?) bundle in the cache.
    pub async fn check_bundle_cache(&mut self) -> eyre::Result<Vec<Bundle>> {
        let bundle_url: Url = Url::parse(&self.config.tx_pool_url)?.join("bundles")?;
        let token = self.authenticator.fetch_oauth_token().await?;

        let result = reqwest::Client::new()
            .get(bundle_url)
            .bearer_auth(token.access_token().secret())
            .send()
            .await?
            .error_for_status()?;

        let body = result.bytes().await?;
        let resp: TxPoolBundleResponse = serde_json::from_slice(&body)?;

        Ok(resp.bundles)
    }

    /// Spawns a task that simply sends out any bundles it ever finds
    pub fn spawn(mut self) -> (UnboundedReceiver<Bundle>, JoinHandle<()>) {
        let (outbound, inbound) = unbounded_channel();
        let jh = tokio::spawn(async move {
            loop {
                if let Ok(bundles) = self.check_bundle_cache().await {
                    tracing::debug!(count = ?bundles.len(), "found bundles");
                    for bundle in bundles.iter() {
                        if let Err(err) = outbound.send(bundle.clone()) {
                            tracing::error!(err = ?err, "Failed to send bundle");
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });
        
        (inbound, jh)
    }
}
