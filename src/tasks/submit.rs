use crate::{
    config::{Provider, ZenithInstance},
    signer::LocalOrAws,
    tasks::block::InProgressBlock,
};
use alloy::{
    consensus::{constants::GWEI_TO_WEI, SimpleCoder},
    eips::BlockNumberOrTag,
    network::{TransactionBuilder, TransactionBuilder4844},
    providers::SendableTx,
    providers::{Provider as _, WalletProvider},
    rpc::types::eth::TransactionRequest,
    signers::Signer,
    sol_types::SolCall,
    transports::TransportError,
};
use alloy_primitives::{FixedBytes, U256};
use alloy_sol_types::SolError;
use eyre::{bail, eyre};
use oauth2::{
    basic::BasicClient, basic::BasicTokenType, reqwest::http_client, AuthUrl, ClientId,
    ClientSecret, EmptyExtraTokenFields, StandardTokenResponse, TokenResponse, TokenUrl,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, error, instrument, trace};
use zenith_types::{
    SignRequest, SignResponse,
    Zenith::{self, IncorrectHostBlock},
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

/// OAuth Audience Claim Name, required param by IdP for client credential grant
const OAUTH_AUDIENCE_CLAIM: &str = "audience";

pub enum ControlFlow {
    Retry,
    Skip,
    Done,
}

/// Submits sidecars in ethereum txns to mainnet ethereum
pub struct SubmitTask {
    /// Ethereum Provider
    pub provider: Provider,

    /// Zenity
    pub zenith: ZenithInstance,

    /// Reqwest
    pub client: reqwest::Client,

    /// Sequencer Signer
    pub sequencer_signer: Option<LocalOrAws>,

    /// Config
    pub config: crate::config::BuilderConfig,
}

impl SubmitTask {
    async fn sup_quincey(&self, sig_request: &SignRequest) -> eyre::Result<SignResponse> {
        tracing::info!(
            host_block_number = %sig_request.host_block_number,
            ru_chain_id = %sig_request.ru_chain_id,
            "pinging quincey for signature"
        );

        let token = self.fetch_oauth_token().await?;

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

    async fn fetch_oauth_token(
        &self,
    ) -> eyre::Result<StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>> {
        let client = BasicClient::new(
            ClientId::new(self.config.oauth_client_id.clone()),
            Some(ClientSecret::new(self.config.oauth_client_secret.clone())),
            AuthUrl::new(self.config.oauth_authenticate_url.clone())?,
            Some(TokenUrl::new(self.config.oauth_token_url.clone())?),
        );

        let token_result = client
            .exchange_client_credentials()
            .add_extra_param(OAUTH_AUDIENCE_CLAIM, self.config.oauth_audience.clone())
            .request(http_client)?;

        Ok(token_result)
    }

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

    fn build_blob_tx(
        &self,
        header: Zenith::BlockHeader,
        v: u8,
        r: FixedBytes<32>,
        s: FixedBytes<32>,
        in_progress: &InProgressBlock,
    ) -> eyre::Result<TransactionRequest> {
        let data = Zenith::submitBlockCall {
            header,
            v,
            r,
            s,
            _4: Default::default(),
        }
        .abi_encode();
        let sidecar = in_progress.encode_blob::<SimpleCoder>().build()?;
        Ok(TransactionRequest::default()
            .with_blob_sidecar(sidecar)
            .with_input(data)
            .with_max_priority_fee_per_gas((GWEI_TO_WEI * 16) as u128))
    }

    async fn next_host_block_height(&self) -> eyre::Result<u64> {
        let result = self.provider.get_block_number().await?;
        let next = result
            .checked_add(1)
            .ok_or_else(|| eyre!("next host block height overflow"))?;
        Ok(next)
    }

    async fn submit_transaction(
        &self,
        resp: &SignResponse,
        in_progress: &InProgressBlock,
    ) -> eyre::Result<ControlFlow> {
        let v: u8 = resp.sig.v().y_parity_byte() + 27;
        let r: FixedBytes<32> = resp.sig.r().into();
        let s: FixedBytes<32> = resp.sig.s().into();

        let header = Zenith::BlockHeader {
            hostBlockNumber: resp.req.host_block_number,
            rollupChainId: U256::from(self.config.ru_chain_id),
            gasLimit: resp.req.gas_limit,
            rewardAddress: resp.req.ru_reward_address,
            blockDataHash: in_progress.contents_hash(),
        };

        let tx = self
            .build_blob_tx(header, v, r, s, in_progress)?
            .with_from(self.provider.default_signer_address())
            .with_to(self.config.zenith_address)
            .with_gas_limit(1_000_000);

        if let Err(TransportError::ErrorResp(e)) = self
            .provider
            .call(&tx)
            .block(BlockNumberOrTag::Pending.into())
            .await
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

        let SendableTx::Envelope(tx) = self.provider.fill(tx).await? else {
            bail!("failed to fill transaction")
        };

        // Send the tx via the primary provider
        let fut = spawn_provider_send!(&self.provider, &tx);

        // Spawn send_tx futures for all additional broadcast providers
        for provider in self.config.connect_additional_broadcast().await? {
            spawn_provider_send!(&provider, &tx);
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
            SignResponse {
                req: sig_request,
                sig,
            }
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
            resp
        };

        self.submit_transaction(&signed, in_progress).await
    }

    /// Spawn the task.
    pub fn spawn(self) -> (mpsc::UnboundedSender<InProgressBlock>, JoinHandle<()>) {
        let (sender, mut inbound) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            loop {
                if let Some(in_progress) = inbound.recv().await {
                    let mut retries = 0;
                    loop {
                        match self.handle_inbound(&in_progress).await {
                            Ok(ControlFlow::Retry) => {
                                retries += 1;
                                if retries > 3 {
                                    tracing::error!(
                                        "error handling inbound block: too many retries"
                                    );
                                    break;
                                }
                                tracing::error!("error handling inbound block: retrying");
                                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                            }
                            Ok(ControlFlow::Skip) => {
                                tracing::info!("skipping block");
                                break;
                            }
                            Ok(ControlFlow::Done) => {
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
