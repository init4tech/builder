use crate::{
    config::{Provider, ZenithInstance},
    signer::LocalOrAws,
    tasks::block::InProgressBlock,
    utils::extract_signature_components,
};
use alloy::{
    consensus::{constants::GWEI_TO_WEI, SimpleCoder},
    eips::BlockNumberOrTag,
    network::{TransactionBuilder, TransactionBuilder4844},
    primitives::{FixedBytes, TxHash, U256},
    providers::{Provider as _, SendableTx, WalletProvider},
    rpc::types::eth::TransactionRequest,
    signers::Signer,
    sol_types::{SolCall, SolError},
    transports::TransportError,
};
use eyre::{bail, eyre};
use metrics::{counter, histogram};
use oauth2::TokenResponse;
use std::time::Instant;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, error, instrument, trace};
use zenith_types::{
    BundleHelper::{self, FillPermit2},
    SignRequest, SignResponse,
    Zenith::IncorrectHostBlock,
};

macro_rules! spawn_provider_send {
    ($provider:expr, $tx:expr) => {
        {
            let p = $provider.clone();
            let t = $tx.clone();
            tokio::spawn(async move {
                p.send_tx_envelope(t).await.inspect_err(|e| {
                    tracing::warn!(%e, "error in transaction broadcast")
                })
            })
        }
    };
}

use super::oauth::Authenticator;

pub enum ControlFlow {
    Retry,
    Skip,
    Done,
}

/// Submits sidecars in ethereum txns to mainnet ethereum
pub struct SubmitTask {
    /// Ethereum Provider
    pub host_provider: Provider,
    /// Zenith
    pub zenith: ZenithInstance,
    /// Reqwest
    pub client: reqwest::Client,
    /// Sequencer Signer
    pub sequencer_signer: Option<LocalOrAws>,
    /// Config
    pub config: crate::config::BuilderConfig,
    /// Authenticator
    pub authenticator: Authenticator,
    // Channel over which to send pending transactions
    pub outbound_tx_channel: mpsc::UnboundedSender<TxHash>,
}

impl SubmitTask {
    async fn sup_quincey(&self, sig_request: &SignRequest) -> eyre::Result<SignResponse> {
        tracing::info!(
            host_block_number = %sig_request.host_block_number,
            ru_chain_id = %sig_request.ru_chain_id,
            "pinging quincey for signature"
        );

        let token = self.authenticator.fetch_oauth_token().await?;

        let resp: reqwest::Response = self
            .client
            .post(self.config.quincey_url.as_ref())
            .json(sig_request)
            .bearer_auth(token.access_token().secret())
            .send()
            .await?
            .error_for_status()?;

        let body = resp.bytes().await?;

        debug!(bytes = body.len(), "retrieved response body");
        trace!(body = %String::from_utf8_lossy(&body), "response body");

        serde_json::from_slice(&body).map_err(Into::into)
    }

    /// Constructs the signing request from the in-progress block passed to it and assigns the
    /// correct height, chain ID, gas limit, and rollup reward address.
    #[instrument(skip_all)]
    async fn construct_sig_request(&self, contents: &InProgressBlock) -> eyre::Result<SignRequest> {
        let ru_chain_id = U256::from(self.config.ru_chain_id);
        let next_block_height = self.next_host_block_height().await?;

        Ok(SignRequest {
            host_block_number: U256::from(next_block_height),
            host_chain_id: U256::from(self.config.host_chain_id),
            ru_chain_id,
            gas_limit: U256::from(self.config.rollup_block_gas_limit),
            ru_reward_address: self.config.builder_rewards_address,
            contents: contents.contents_hash(),
        })
    }

    /// Builds blob transaction from the provided header and signature values
    fn build_blob_tx(
        &self,
        fills: Vec<FillPermit2>,
        header: BundleHelper::BlockHeader,
        v: u8,
        r: FixedBytes<32>,
        s: FixedBytes<32>,
        in_progress: &InProgressBlock,
    ) -> eyre::Result<TransactionRequest> {
        let data = zenith_types::BundleHelper::submitCall { fills, header, v, r, s }.abi_encode();

        let sidecar = in_progress.encode_blob::<SimpleCoder>().build()?;
        Ok(TransactionRequest::default()
            .with_blob_sidecar(sidecar)
            .with_input(data)
            .with_max_priority_fee_per_gas((GWEI_TO_WEI * 16) as u128))
    }

    /// Returns the next host block height
    async fn next_host_block_height(&self) -> eyre::Result<u64> {
        let result = self.host_provider.get_block_number().await?;
        let next = result.checked_add(1).ok_or_else(|| eyre!("next host block height overflow"))?;
        Ok(next)
    }

    /// Submits the EIP 4844 transaction to the network
    async fn submit_transaction(
        &self,
        resp: &SignResponse,
        in_progress: &InProgressBlock,
    ) -> eyre::Result<ControlFlow> {
        let (v, r, s) = extract_signature_components(&resp.sig);

        let header = zenith_types::BundleHelper::BlockHeader {
            hostBlockNumber: resp.req.host_block_number,
            rollupChainId: U256::from(self.config.ru_chain_id),
            gasLimit: resp.req.gas_limit,
            rewardAddress: resp.req.ru_reward_address,
            blockDataHash: in_progress.contents_hash(),
        };

        let fills = vec![]; // NB: ignored until fills are implemented
        let tx = self
            .build_blob_tx(fills, header, v, r, s, in_progress)?
            .with_from(self.host_provider.default_signer_address())
            .with_to(self.config.zenith_address)
            .with_gas_limit(1_000_000);

        if let Err(TransportError::ErrorResp(e)) =
            self.host_provider.call(&tx).block(BlockNumberOrTag::Pending.into()).await
        {
            error!(
                code = e.code,
                message = %e.message,
                data = ?e.data,
                "error in transaction submission"
            );

            if e.as_revert_data() == Some(IncorrectHostBlock::SELECTOR.into()) {
                return Ok(ControlFlow::Retry);
            }

            return Ok(ControlFlow::Skip);
        }

        // All validation checks have passed, send the transaction
        self.send_transaction(resp, tx).await
    }

    async fn send_transaction(
        &self,
        resp: &SignResponse,
        tx: TransactionRequest,
    ) -> Result<ControlFlow, eyre::Error> {
        tracing::debug!(
            host_block_number = %resp.req.host_block_number,
            gas_limit = %resp.req.gas_limit,
            "sending transaction to network"
        );

        let SendableTx::Envelope(tx) = self.host_provider.fill(tx).await? else {
            bail!("failed to fill transaction")
        };

        // Send the tx via the primary host_provider
        let fut = spawn_provider_send!(&self.host_provider, &tx);

        // Spawn send_tx futures for all additional broadcast host_providers
        for host_provider in self.config.connect_additional_broadcast().await? {
            spawn_provider_send!(&host_provider, &tx);
        }

        // send the in-progress transaction over the outbound_tx_channel
        if self.outbound_tx_channel.send(*tx.tx_hash()).is_err() {
            tracing::error!("receipts task gone");
        }

        // question mark unwraps join error, which would be an internal panic
        // then if let checks for rpc error
        if let Err(e) = fut.await? {
            tracing::error!(error = %e, "Primary tx broadcast failed. Skipping transaction.");
            return Ok(ControlFlow::Skip);
        }

        tracing::info!(
            tx_hash = %tx.tx_hash(),
            ru_chain_id = %resp.req.ru_chain_id,
            gas_limit = %resp.req.gas_limit,
            "dispatched to network"
        );

        Ok(ControlFlow::Done)
    }

    #[instrument(skip_all, err)]
    async fn handle_inbound(&self, in_progress: &InProgressBlock) -> eyre::Result<ControlFlow> {
        tracing::info!(txns = in_progress.len(), "handling inbound block");
        let sig_request = match self.construct_sig_request(in_progress).await {
            Ok(sig_request) => sig_request,
            Err(e) => {
                tracing::error!(error = %e, "error constructing signature request");
                return Ok(ControlFlow::Skip);
            }
        };

        tracing::debug!(
            host_block_number = %sig_request.host_block_number,
            ru_chain_id = %sig_request.ru_chain_id,
            "constructed signature request for host block"
        );

        // If configured with a local signer, we use it. Otherwise, we ask
        // quincey (politely)
        let signed = if let Some(signer) = &self.sequencer_signer {
            let sig = signer.sign_hash(&sig_request.signing_hash()).await?;
            tracing::debug!(
                sig = hex::encode(sig.as_bytes()),
                "acquired signature from local signer"
            );
            SignResponse { req: sig_request, sig }
        } else {
            let resp: SignResponse = match self.sup_quincey(&sig_request).await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::error!(error = %e, "error acquiring signature from quincey");
                    return Ok(ControlFlow::Retry);
                }
            };
            tracing::debug!(
                sig = hex::encode(resp.sig.as_bytes()),
                "acquired signature from quincey"
            );
            counter!("builder.quincey_signature_acquired").increment(1);
            resp
        };

        self.submit_transaction(&signed, in_progress).await
    }

    /// Spawns the in progress block building task
    pub fn spawn(self) -> (mpsc::UnboundedSender<InProgressBlock>, JoinHandle<()>) {
        let (sender, mut inbound) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            loop {
                if let Some(in_progress) = inbound.recv().await {
                    let building_start_time = Instant::now();
                    let mut retries = 0;
                    loop {
                        match self.handle_inbound(&in_progress).await {
                            Ok(ControlFlow::Retry) => {
                                retries += 1;
                                if retries > 3 {
                                    counter!("builder.building_too_many_retries").increment(1);
                                    histogram!("builder.block_build_time")
                                        .record(building_start_time.elapsed().as_millis() as f64);
                                    tracing::error!(
                                        "error handling inbound block: too many retries"
                                    );
                                    break;
                                }
                                tracing::error!("error handling inbound block: retrying");
                                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                            }
                            Ok(ControlFlow::Skip) => {
                                histogram!("builder.block_build_time")
                                    .record(building_start_time.elapsed().as_millis() as f64);
                                counter!("builder.skipped_blocks").increment(1);
                                tracing::info!("skipping block");
                                break;
                            }
                            Ok(ControlFlow::Done) => {
                                histogram!("builder.block_build_time")
                                    .record(building_start_time.elapsed().as_millis() as f64);
                                counter!("builder.submitted_successful_blocks").increment(1);
                                tracing::info!("block landed successfully");
                                break;
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "error handling inbound block");
                                break;
                            }
                        }
                    }
                } else {
                    tracing::debug!("upstream task gone");
                    break;
                }
            }
        });

        (sender, handle)
    }
}
