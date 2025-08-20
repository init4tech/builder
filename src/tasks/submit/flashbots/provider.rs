//! A generic Flashbots bundle API wrapper.
use crate::config::{BuilderConfig, HostProvider};
use alloy::{
    primitives::BlockNumber,
    providers::Provider,
    rpc::types::mev::{EthBundleHash, MevSendBundle},
};
use eyre::Context as _;
use init4_bin_base::deps::tracing::debug;
use serde_json::json;
use signet_zenith::Zenith::ZenithInstance;

/// A wrapper over a `Provider` that adds Flashbots MEV bundle helpers.
#[derive(Debug)]
pub struct FlashbotsProvider {
    /// The base URL for the Flashbots API.
    pub relay_url: url::Url,
    /// Zenith instance for constructing Signet blocks.
    pub zenith: ZenithInstance<HostProvider>,
    /// Builder configuration for the task.
    pub config: BuilderConfig,
}

impl FlashbotsProvider {
    /// Wraps a provider with the URL and returns a new `FlashbotsProvider`.
    pub fn new(
        relay_url: url::Url,
        zenith: ZenithInstance<HostProvider>,
        config: &BuilderConfig,
    ) -> Self {
        Self { relay_url, zenith, config: config.clone() }
    }

    /// Submit the prepared Flashbots bundle to the relay via `mev_sendBundle`.
    pub async fn send_bundle(&self, bundle: MevSendBundle) -> eyre::Result<EthBundleHash> {
        // NB: The Flashbots relay expects a single parameter which is the bundle object.
        // Alloy's `raw_request` accepts any serializable params; wrapping in a 1-tuple is fine.
        let hash: EthBundleHash = self
            .zenith
            .provider()
            .raw_request("mev_sendBundle".into(), (bundle,))
            .await
            .wrap_err("mev_sendBundle RPC failed")?;
        debug!(?hash, "mev_sendBundle response");
        Ok(hash)
    }

    /// Simulate a bundle via `mev_simBundle`.
    pub async fn simulate_bundle(&self, bundle: MevSendBundle) -> eyre::Result<()> {
        let resp: serde_json::Value = self
            .zenith
            .provider()
            .raw_request("mev_simBundle".into(), (bundle.clone(),))
            .await
            .wrap_err("mev_simBundle RPC failed")?;
        debug!(?resp, "mev_simBundle response");
        Ok(())
    }

    /// Check that status of a bundle
    pub async fn bundle_status(
        &self,
        _hash: EthBundleHash,
        block_number: BlockNumber,
    ) -> eyre::Result<()> {
        let params = json!({ "bundleHash": _hash, "blockNumber": block_number });
        let resp: serde_json::Value = self
            .zenith
            .provider()
            .raw_request("flashbots_getBundleStatsV2".into(), (params,))
            .await
            .wrap_err("flashbots_getBundleStatsV2 RPC failed")?;
        debug!(?resp, "flashbots_getBundleStatsV2 response");
        Ok(())
    }
}
