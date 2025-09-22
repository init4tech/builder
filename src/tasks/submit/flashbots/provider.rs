//! A generic Flashbots bundle API wrapper.
use std::borrow::Cow;

use crate::config::BuilderConfig;
use alloy::{
    primitives::{BlockNumber, keccak256},
    rpc::{
        json_rpc::{Id, Response, ResponsePayload, RpcRecv, RpcSend},
        types::mev::{EthBundleHash, MevSendBundle, SimBundleResponse},
    },
    signers::Signer,
};
use init4_bin_base::utils::signer::LocalOrAws;
use reqwest::header::CONTENT_TYPE;
use serde_json::json;

/// An RPC connection to relevant Flashbots endpoints.
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
    pub async fn send_bundle(&self, bundle: &MevSendBundle) -> eyre::Result<EthBundleHash> {
        self.raw_call("mev_sendBundle", &[bundle]).await
    }

    /// Simulate a bundle via `mev_simBundle`.
    pub async fn simulate_bundle(&self, bundle: &MevSendBundle) -> eyre::Result<()> {
        let resp: SimBundleResponse = self.raw_call("mev_simBundle", &[bundle]).await?;
        dbg!("successfully simulated bundle", &resp);
        Ok(())
    }

    /// Fetches the bundle status by hash
    pub async fn bundle_status(
        &self,
        hash: EthBundleHash,
        block_number: BlockNumber,
    ) -> eyre::Result<()> {
        let params = json!({ "bundleHash": hash, "blockNumber": block_number });
        let _resp: serde_json::Value =
            self.raw_call("flashbots_getBundleStatsV2", &[params]).await?;

        Ok(())
    }

    /// Makes a raw JSON-RPC call with the Flashbots signature header to the method with the given params.
    async fn raw_call<Params: RpcSend, Payload: RpcRecv>(
        &self,
        method: &str,
        params: &Params,
    ) -> eyre::Result<Payload> {
        let req = alloy::rpc::json_rpc::Request::new(
            Cow::Owned(method.to_string()),
            Id::Number(1),
            params,
        );
        let body_bz = serde_json::to_vec(&req)?;
        drop(req);

        let value = self.compute_signature(&body_bz).await?;

        let client = reqwest::Client::new();
        let resp = client
            .post(self.relay_url.as_str())
            .header(CONTENT_TYPE, "application/json")
            .header("X-Flashbots-Signature", value)
            .body(body_bz)
            .send()
            .await?;

        let resp: Response<Payload> = resp.json().await?;

        match resp.payload {
            ResponsePayload::Success(payload) => Ok(payload),
            ResponsePayload::Failure(err) => {
                eyre::bail!("flashbots error: {err}");
            }
        }
    }

    /// Builds an EIP-191 signature for the given body bytes.
    async fn compute_signature(&self, body_bz: &[u8]) -> Result<String, eyre::Error> {
        let payload = keccak256(body_bz).to_string();
        let signature = self.signer.sign_message(payload.as_ref()).await?;
        dbg!(signature.to_string());
        let address = self.signer.address();
        let value = format!("{address}:{signature}");
        Ok(value)
    }
}
