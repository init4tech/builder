//! A generic Flashbots bundle API wrapper.
use crate::config::BuilderConfig;
use alloy::{
    primitives::BlockNumber,
    rpc::types::mev::{EthBundleHash, MevSendBundle},
};
use eyre::Context as _;
use eyre::eyre;
use init4_bin_base::deps::tracing::debug;
use reqwest::Client as HttpClient;
use serde_json::json;

/// A wrapper over a `Provider` that adds Flashbots MEV bundle helpers.
#[derive(Debug)]
pub struct FlashbotsProvider {
    /// The base URL for the Flashbots API.
    pub relay_url: url::Url,
    /// Inner HTTP client used for JSON-RPC requests to the relay.
    pub inner: HttpClient,
    /// Builder configuration for the task.
    pub config: BuilderConfig,
}

impl FlashbotsProvider {
    /// Wraps a provider with the URL and returns a new `FlashbotsProvider`.
    pub fn new(config: &BuilderConfig) -> Self {
        let relay_url =
            config.flashbots_endpoint.as_ref().expect("Flashbots endpoint must be set").clone();
        Self { relay_url, inner: HttpClient::new(), config: config.clone() }
    }

    /// Submit the prepared Flashbots bundle to the relay via `mev_sendBundle`.
    pub async fn send_bundle(&self, bundle: MevSendBundle) -> eyre::Result<EthBundleHash> {
        // NB: The Flashbots relay expects a single parameter which is the bundle object.
        // Alloy's `raw_request` accepts any serializable params; wrapping in a 1-tuple is fine.
        // We POST a JSON-RPC request to the relay URL using our inner HTTP client.
        let body =
            json!({ "jsonrpc": "2.0", "id": 1, "method": "mev_sendBundle", "params": [bundle] });
        let resp = self
            .inner
            .post(self.relay_url.as_str())
            .json(&body)
            .send()
            .await
            .wrap_err("mev_sendBundle HTTP request failed")?;

        let v: serde_json::Value =
            resp.json().await.wrap_err("failed to parse mev_sendBundle response")?;
        if let Some(err) = v.get("error") {
            return Err(eyre!("mev_sendBundle error: {}", err));
        }
        let result = v.get("result").ok_or_else(|| eyre!("mev_sendBundle missing result"))?;
        let hash: EthBundleHash = serde_json::from_value(result.clone())
            .wrap_err("failed to deserialize mev_sendBundle result")?;
        debug!(?hash, "mev_sendBundle response");
        Ok(hash)
    }

    /// Simulate a bundle via `mev_simBundle`.
    pub async fn simulate_bundle(&self, bundle: MevSendBundle) -> eyre::Result<()> {
        let body =
            json!({ "jsonrpc": "2.0", "id": 1, "method": "mev_simBundle", "params": [bundle] });
        let resp = self
            .inner
            .post(self.relay_url.as_str())
            .json(&body)
            .send()
            .await
            .wrap_err("mev_simBundle HTTP request failed")?;

        let v: serde_json::Value =
            resp.json().await.wrap_err("failed to parse mev_simBundle response")?;
        if let Some(err) = v.get("error") {
            return Err(eyre!("mev_simBundle error: {}", err));
        }
        debug!(?v, "mev_simBundle response");
        Ok(())
    }

    /// Check that status of a bundle
    pub async fn bundle_status(
        &self,
        _hash: EthBundleHash,
        block_number: BlockNumber,
    ) -> eyre::Result<()> {
        let params = json!({ "bundleHash": _hash, "blockNumber": block_number });
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "flashbots_getBundleStatsV2", "params": [params] });
        let resp = self
            .inner
            .post(self.relay_url.as_str())
            .json(&body)
            .send()
            .await
            .wrap_err("flashbots_getBundleStatsV2 HTTP request failed")?;

        let v: serde_json::Value =
            resp.json().await.wrap_err("failed to parse flashbots_getBundleStatsV2 response")?;
        if let Some(err) = v.get("error") {
            return Err(eyre!("flashbots_getBundleStatsV2 error: {}", err));
        }
        debug!(?v, "flashbots_getBundleStatsV2 response");
        Ok(())
    }
}

// (no additional helpers)
