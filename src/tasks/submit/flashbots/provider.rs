//! A generic Flashbots bundle API wrapper.
use crate::config::BuilderConfig;
use alloy::{
    primitives::{BlockNumber, keccak256},
    rpc::types::mev::{EthBundleHash, MevSendBundle, SimBundleResponse},
    signers::Signer,
};
use eyre::Context as _;
use init4_bin_base::utils::signer::LocalOrAws;
use reqwest::header::CONTENT_TYPE;
use serde_json::json;

/// A wrapper over a `Provider` that adds Flashbots MEV bundle helpers.
#[derive(Debug)]
pub struct Flashbots {
    /// The base URL for the Flashbots API.
    pub relay_url: url::Url,
    /// Builder configuration for the task.
    pub config: BuilderConfig,
    /// Signer is loaded once at startup.
    signer: LocalOrAws,
}

impl Flashbots {
    /// Wraps a provider with the URL and returns a new `FlashbotsProvider`.
    pub async fn new(config: &BuilderConfig) -> Self {
        let relay_url =
            config.flashbots_endpoint.as_ref().expect("Flashbots endpoint must be set").clone();

        let signer =
            config.connect_builder_signer().await.expect("Local or AWS signer must be set");

        Self { relay_url, config: config.clone(), signer }
    }

    /// Sends a bundle  via `mev_sendBundle`.
    pub async fn send_bundle(&self, bundle: MevSendBundle) -> eyre::Result<EthBundleHash> {
        let params = serde_json::to_value(bundle)?;
        let v = self.raw_call("mev_sendBundle", params).await?;
        let hash: EthBundleHash =
            serde_json::from_value(v.get("result").cloned().unwrap_or(serde_json::Value::Null))?;
        Ok(hash)
    }

    /// Simulate a bundle via `mev_simBundle`.
    pub async fn simulate_bundle(&self, bundle: MevSendBundle) -> eyre::Result<()> {
        let params = serde_json::to_value(bundle)?;
        let v = self.raw_call("mev_simBundle", params).await?;
        let resp: SimBundleResponse =
            serde_json::from_value(v.get("result").cloned().unwrap_or(serde_json::Value::Null))?;
        dbg!("successfully simulated bundle", &resp);
        Ok(())
    }

    /// Fetches the bundle status by hash
    pub async fn bundle_status(
        &self,
        _hash: EthBundleHash,
        block_number: BlockNumber,
    ) -> eyre::Result<()> {
        let params = json!({ "bundleHash": _hash, "blockNumber": block_number });
        let _ = self.raw_call("flashbots_getBundleStatsV2", params).await?;
        Ok(())
    }

    /// Makes a raw JSON-RPC call with the Flashbots signature header to the method with the given params.
    async fn raw_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> eyre::Result<serde_json::Value> {
        let params = match params {
            serde_json::Value::Array(_) => params,
            other => serde_json::Value::Array(vec![other]),
        };

        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let body_bz = serde_json::to_vec(&body)?;

        let value = self.compute_signature(&body_bz).await?;

        let client = reqwest::Client::new();
        let resp = client
            .post(self.relay_url.as_str())
            .header(CONTENT_TYPE, "application/json")
            .header("X-Flashbots-Signature", value)
            .body(body_bz)
            .send()
            .await?;

        let text = resp.text().await?;
        let v: serde_json::Value =
            serde_json::from_str(&text).wrap_err("failed to parse flashbots JSON")?;
        if let Some(err) = v.get("error") {
            eyre::bail!("flashbots error: {err}");
        }
        Ok(v)
    }

    /// Builds an EIP-191 signature for the given body bytes.
    async fn compute_signature(&self, body_bz: &[u8]) -> Result<String, eyre::Error> {
        let payload = format!("0x{:x}", keccak256(body_bz));
        let signature = self.signer.sign_message(payload.as_ref()).await?;
        dbg!(signature.to_string());
        let address = self.signer.address();
        let value = format!("{address}:{signature}");
        Ok(value)
    }
}
