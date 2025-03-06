//! Bundler service responsible for managing bundles.
use super::oauth::Authenticator;

pub use crate::config::BuilderConfig;

use oauth2::TokenResponse;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
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
    pub seen_uuids: HashMap<String, Instant>,
}

/// Implements a poller for the block builder to pull bundles from the tx cache.
impl BundlePoller {
    /// Creates a new BundlePoller from the provided builder config.
    pub fn new(config: &BuilderConfig, authenticator: Authenticator) -> Self {
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
    pub fn evict(&mut self) {
        let expired_keys: Vec<String> = self
            .seen_uuids
            .iter()
            .filter_map(
                |(key, expiry)| {
                    if expiry.elapsed().is_zero() { Some(key.clone()) } else { None }
                },
            )
            .collect();

        for key in expired_keys {
            self.seen_uuids.remove(&key);
        }
    }
}
