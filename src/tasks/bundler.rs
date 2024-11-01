//! Bundler service responsible for polling and submitting bundles to the in-progress block.
use std::time::Duration;

pub use crate::config::BuilderConfig;
use alloy_sol_types::abi::token;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use signet_types::SignetEthBundle;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::debug;

use oauth2::{
    basic::BasicClient, basic::BasicTokenType, reqwest::http_client, AuthUrl, ClientId,
    ClientSecret, EmptyExtraTokenFields, StandardTokenResponse, TokenResponse, TokenUrl,
};

use super::oauth::Authenticator;

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

pub struct BundlePoller {
    pub config: BuilderConfig,
    pub authenticator: Authenticator,
}

/// Implements a poller for the block builder to pull bundles from the tx cache.
impl BundlePoller {
    /// Creates a new BundlePoller from the provided builder config.
    pub async fn new(config: &BuilderConfig, authenticator: Authenticator) -> Self {
        Self {
            config: config.clone(),
            authenticator,
        }
    }

    /// Fetches bundles from the transaction cache and returns the (oldest? random?) bundle in the cache.
    pub async fn check_bundle_cache(&mut self) -> eyre::Result<Option<Bundle>> {
        // Fetch a token from the authenticator if not currently authenticated
        if !self.authenticator.is_authenticated().await {
            self.authenticator.authenticate().await?;
        }

        let bundle_url: Url = Url::parse(&self.config.tx_pool_url)?.join("bundles")?;
        let token = self.authenticator.token().await?;

        // Add the token to the request headers
        let result = reqwest::Client::new()
            .get(bundle_url)
            .bearer_auth(token.access_token().secret())
            .send()
            .await?
            .error_for_status()?;

        let body = result.bytes().await?;
        tracing::debug!(bytes = body.len(), "retrieved response body");
        tracing::trace!(body = %String::from_utf8_lossy(&body), "response body");
        serde_json::from_slice(&body).map_err(Into::into)
    }

    pub fn spawn(mut self, bundle_channel: mpsc::UnboundedSender<Bundle>) -> JoinHandle<()> {
        let handle: JoinHandle<()> = tokio::spawn(async move {
            loop {
                let bundle_channel = bundle_channel.clone();
                let bundles = self.check_bundle_cache().await;

                match bundles {
                    Ok(Some(bundle)) => {
                        let result = bundle_channel.send(bundle);
                        if result.is_err() {
                            tracing::debug!("bundle_channel failed to send bundle");
                        }
                    }
                    Ok(None) => {
                        debug!("no bundles found in tx-pool");
                    }
                    Err(err) => {
                        debug!(?err, "error fetching bundles from tx-pool");
                    }
                }

                tokio::time::sleep(Duration::from_secs(self.config.tx_pool_poll_interval)).await;
            }
        });

        handle
    }
}
