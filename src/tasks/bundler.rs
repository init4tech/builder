pub use crate::config::BuilderConfig;
use eyre::Error;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use signet_types::SignetEthBundle;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, info};

// TODO: Consider exporting this type from the signet-types crate instead of duplicating it here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub id: String,
    pub bundle: SignetEthBundle,
}

/// Response from the tx-pool containing a list of bundles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxPoolBundleResponse {
    pub bundles: Vec<Bundle>,
}

pub struct BundleFetcher {
    pub config: BuilderConfig,
}

/// Implements a poller for the block builder to pull bundles from the tx cache.
impl BundleFetcher {
    /// Creates a new BundleFetcher from the provided builder config.
    pub fn new(config: &BuilderConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    /// Fetches bundles from the transaction cache and returns the (oldest? random?) bundle in the cache.
    pub async fn check_bundle_cache(&mut self) -> Result<Bundle, Error> {
        let bundle_url: Url = Url::parse(&self.config.tx_pool_url)?.join("bundles")?;
        let result = reqwest::get(bundle_url).await?;
        let response: TxPoolBundleResponse = from_slice(result.text().await?.as_bytes())?;
        match response.bundles.into_iter().next() {
            Some(bundle) => Ok(bundle),
            None => Err(eyre::eyre!("no bundles found in tx-pool")),
        }
    }
    
    pub fn spawn(mut self, bundle_channel: mpsc::UnboundedSender<Bundle>) -> JoinHandle<()> {

        let handle: JoinHandle<()> = tokio::spawn(async move {

            loop {

                let bundle_channel = bundle_channel.clone();
                let bundles = self.check_bundle_cache().await;

                match bundles {
                    Ok(bundle) => {
                        debug!("bundles found in tx-pool");
                        let result = bundle_channel.send(bundle);
                        if result.is_err() {
                            tracing::debug!("bundle_channel failed to send bundle");
                            continue;
                        }
                    },
                    Err(err) => {
                        debug!(?err, "no bundles found in tx-pool");
                    },
                }
            }

        });

        handle
    }
}
