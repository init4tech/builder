//! Bundler service responsible for polling and submitting bundles to the in-progress block.
use std::time::{Duration, Instant};

pub use crate::config::BuilderConfig;
use alloy_primitives::map::HashMap;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use signet_types::SignetEthBundle;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::debug;

use oauth2::TokenResponse;

use super::oauth::Authenticator;

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
    pub seen_uuids: HashMap<String, Instant>,
}

/// Implements a poller for the block builder to pull bundles from the tx cache.
impl BundlePoller {
    /// Creates a new BundlePoller from the provided builder config.
    pub async fn new(config: &BuilderConfig, authenticator: Authenticator) -> Self {
        Self { config: config.clone(), authenticator, seen_uuids: HashMap::new() }
    }

    /// Fetches bundles from the transaction cache and returns the (oldest? random?) bundle in the cache.
    pub async fn check_bundle_cache(&mut self) -> eyre::Result<Vec<Bundle>> {
        let mut unique: Vec<Bundle> = Vec::new();

        let bundle_url: Url = Url::parse(&self.config.tx_pool_url)?.join("bundles")?;
        let token = self.authenticator.fetch_oauth_token().await?;

        // Add the token to the request headers
        let result = reqwest::Client::new()
            .get(bundle_url)
            .bearer_auth(token.access_token().secret())
            .send()
            .await?
            .error_for_status()?;

        let body = result.bytes().await?;
        let bundles: TxPoolBundleResponse = serde_json::from_slice(&body)?;

        bundles.bundles.iter().for_each(|bundle| {
            self.check_seen_bundles(bundle.clone(), &mut unique);
        });

        Ok(unique)
    }

    /// Checks if the bundle has been seen before and if not, adds it to the unique bundles list.
    fn check_seen_bundles(&mut self, bundle: Bundle, unique: &mut Vec<Bundle>) {
        self.seen_uuids.entry(bundle.id.clone()).or_insert_with(|| {
            // add to the set of unique bundles
            unique.push(bundle.clone());
            Instant::now() + Duration::from_secs(self.config.tx_pool_cache_duration)
        });
    }

    /// Evicts expired bundles from the cache.
    fn evict(&mut self) {
        let expired_keys: Vec<String> = self
            .seen_uuids
            .iter()
            .filter_map(
                |(key, expiry)| {
                    if expiry.elapsed().is_zero() {
                        Some(key.clone())
                    } else {
                        None
                    }
                },
            )
            .collect();

        for key in expired_keys {
            self.seen_uuids.remove(&key);
        }
    }

    pub fn spawn(mut self, bundle_channel: mpsc::UnboundedSender<Bundle>) -> JoinHandle<()> {
        let handle: JoinHandle<()> = tokio::spawn(async move {
            loop {
                let bundle_channel = bundle_channel.clone();
                let bundles = self.check_bundle_cache().await;

                match bundles {
                    Ok(bundles) => {
                        for bundle in bundles {
                            let result = bundle_channel.send(bundle);
                            if result.is_err() {
                                tracing::debug!("bundle_channel failed to send bundle");
                            }
                        }
                    }
                    Err(err) => {
                        debug!(?err, "error fetching bundles from tx-pool");
                    }
                }

                // evict expired bundles once every loop
                self.evict();

                tokio::time::sleep(Duration::from_secs(self.config.tx_pool_poll_interval)).await;
            }
        });

        handle
    }
}
