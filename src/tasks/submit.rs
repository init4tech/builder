use crate::{
    config::{HostProvider, ZenithInstance},
    quincey::Quincey,
    utils::extract_signature_components,
};
use alloy::{
    consensus::{SimpleCoder, constants::GWEI_TO_WEI},
    eips::BlockNumberOrTag,
    network::{TransactionBuilder, TransactionBuilder4844},
    primitives::{FixedBytes, TxHash, U256},
    providers::{Provider as _, SendableTx, WalletProvider},
    rpc::types::eth::TransactionRequest,
    sol_types::{SolCall, SolError},
    transports::TransportError,
};
use eyre::{bail, eyre};
use init4_bin_base::deps::{
    metrics::{counter, histogram},
    tracing::{self, Instrument, debug, debug_span, error, info, instrument, warn},
};
use signet_sim::BuiltBlock;
use signet_types::{SignRequest, SignResponse};
use signet_zenith::{
    BundleHelper::{self, BlockHeader, FillPermit2, submitCall},
    Zenith::IncorrectHostBlock,
};
use std::time::Instant;
use tokio::{sync::mpsc, task::JoinHandle};

macro_rules! spawn_provider_send {
    ($provider:expr, $tx:expr) => {
        {
            let p = $provider.clone();
            let t = $tx.clone();
            tokio::spawn(async move {
                p.send_tx_envelope(t).await.inspect_err(|e| {
                   warn!(%e, "error in transaction broadcast")
                })
            })
        }
    };
}

/// Control flow for transaction submission.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ControlFlow {
    /// Retry
    Retry,
    /// Skip
    Skip,
    /// Succesfully submitted
    Done,
}

/// Submits sidecars in ethereum txns to mainnet ethereum
#[derive(Debug)]
pub struct SubmitTask {
    /// Zenith
    pub zenith: ZenithInstance,

    /// Quincey
    pub quincey: Quincey,

    /// Config
    pub config: crate::config::BuilderConfig,

    /// Channel over which to send pending transactions
    pub outbound_tx_channel: mpsc::UnboundedSender<TxHash>,
}

impl SubmitTask {
    /// Get the provider from the zenith instance
    const fn provider(&self) -> &HostProvider {
        self.zenith.provider()
    }

    /// Constructs the signing request from the in-progress block passed to it and assigns the
    /// correct height, chain ID, gas limit, and rollup reward address.
    #[instrument(skip_all)]
    async fn construct_sig_request(&self, contents: &BuiltBlock) -> eyre::Result<SignRequest> {
        let ru_chain_id = U256::from(self.config.ru_chain_id);
        let next_block_height = self.next_host_block_height().await?;

        Ok(SignRequest {
            host_block_number: U256::from(next_block_height),
            host_chain_id: U256::from(self.config.host_chain_id),
            ru_chain_id,
            gas_limit: U256::from(self.config.rollup_block_gas_limit),
            ru_reward_address: self.config.builder_rewards_address,
            contents: *contents.contents_hash(),
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
        in_progress: &BuiltBlock,
    ) -> eyre::Result<TransactionRequest> {
        let data = submitCall { fills, header, v, r, s }.abi_encode();

        let sidecar = in_progress.encode_blob::<SimpleCoder>().build()?;
        Ok(TransactionRequest::default()
            .with_blob_sidecar(sidecar)
            .with_input(data)
            .with_max_priority_fee_per_gas((GWEI_TO_WEI * 16) as u128))
    }

    /// Returns the next host block height
    async fn next_host_block_height(&self) -> eyre::Result<u64> {
        let result = self.provider().get_block_number().await?;
        let next = result.checked_add(1).ok_or_else(|| eyre!("next host block height overflow"))?;
        Ok(next)
    }

    /// Submits the EIP 4844 transaction to the network
    async fn submit_transaction(
        &self,
        resp: &SignResponse,
        in_progress: &BuiltBlock,
    ) -> eyre::Result<ControlFlow> {
        let (v, r, s) = extract_signature_components(&resp.sig);

        let header = BlockHeader {
            hostBlockNumber: resp.req.host_block_number,
            rollupChainId: U256::from(self.config.ru_chain_id),
            gasLimit: resp.req.gas_limit,
            rewardAddress: resp.req.ru_reward_address,
            blockDataHash: *in_progress.contents_hash(),
        };

        let fills = vec![]; // NB: ignored until fills are implemented
        let tx = self
            .build_blob_tx(fills, header, v, r, s, in_progress)?
            .with_from(self.provider().default_signer_address())
            .with_to(self.config.builder_helper_address)
            .with_gas_limit(1_000_000);

        if let Err(TransportError::ErrorResp(e)) =
            self.provider().call(tx.clone()).block(BlockNumberOrTag::Pending.into()).await
        {
            error!(
                code = e.code,
                message = %e.message,
                data = ?e.data,
                "error in transaction submission"
            );

            if e.as_revert_data()
                .map(|data| data.starts_with(&IncorrectHostBlock::SELECTOR))
                .unwrap_or_default()
            {
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
        debug!(
            host_block_number = %resp.req.host_block_number,
            gas_limit = %resp.req.gas_limit,
            "sending transaction to network"
        );

        let SendableTx::Envelope(tx) = self.provider().fill(tx).await? else {
            bail!("failed to fill transaction")
        };

        // Send the tx via the primary host_provider
        let fut = spawn_provider_send!(self.provider(), &tx);

        // Spawn send_tx futures for all additional broadcast host_providers
        for host_provider in self.config.connect_additional_broadcast() {
            spawn_provider_send!(&host_provider, &tx);
        }

        // send the in-progress transaction over the outbound_tx_channel
        if self.outbound_tx_channel.send(*tx.tx_hash()).is_err() {
            error!("receipts task gone");
        }

        // question mark unwraps join error, which would be an internal panic
        // then if let checks for rpc error
        if let Err(e) = fut.await? {
            error!(error = %e, "Primary tx broadcast failed. Skipping transaction.");
            return Ok(ControlFlow::Skip);
        }

        info!(
            tx_hash = %tx.tx_hash(),
            ru_chain_id = %resp.req.ru_chain_id,
            gas_limit = %resp.req.gas_limit,
            "dispatched to network"
        );

        Ok(ControlFlow::Done)
    }

    #[instrument(skip_all)]
    async fn handle_inbound(&self, block: &BuiltBlock) -> eyre::Result<ControlFlow> {
        info!(txns = block.tx_count(), "handling inbound block");
        let Ok(sig_request) = self.construct_sig_request(block).await.inspect_err(|e| {
            error!(error = %e, "error constructing signature request");
        }) else {
            return Ok(ControlFlow::Skip);
        };

        debug!(
            host_block_number = %sig_request.host_block_number,
            ru_chain_id = %sig_request.ru_chain_id,
            "constructed signature request for host block"
        );

        let signed = self.quincey.get_signature(&sig_request).await?;

        self.submit_transaction(&signed, block).await
    }

    async fn retrying_handle_inbound(
        &self,
        block: &BuiltBlock,
        retry_limit: usize,
    ) -> eyre::Result<ControlFlow> {
        let mut retries = 0;
        let building_start_time = Instant::now();

        let result = loop {
            let span = debug_span!("SubmitTask::retrying_handle_inbound", retries);

            let result =
                self.handle_inbound(block).instrument(span.clone()).await.inspect_err(|e| {
                    error!(error = %e, "error handling inbound block");
                })?;

            let guard = span.entered();

            match result {
                ControlFlow::Retry => {
                    retries += 1;
                    if retries > retry_limit {
                        counter!("builder.building_too_many_retries").increment(1);
                        return Ok(ControlFlow::Skip);
                    }
                    error!("error handling inbound block: retrying");
                    drop(guard);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

                    continue;
                }
                ControlFlow::Skip => {
                    counter!("builder.skipped_blocks").increment(1);
                    break result;
                }
                ControlFlow::Done => {
                    counter!("builder.submitted_successful_blocks").increment(1);
                    break result;
                }
            }
        };

        // This is reached when `Done` or `Skip` is returned
        histogram!("builder.block_build_time")
            .record(building_start_time.elapsed().as_millis() as f64);
        info!(?result, "finished block building");
        Ok(result)
    }

    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<BuiltBlock>) {
        loop {
            let Some(block) = inbound.recv().await else {
                debug!("upstream task gone");
                break;
            };

            if self.retrying_handle_inbound(&block, 3).await.is_err() {
                continue;
            }
        }
    }

    /// Spawns the in progress block building task
    pub fn spawn(self) -> (mpsc::UnboundedSender<BuiltBlock>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel();
        let handle = tokio::spawn(self.task_future(inbound));

        (sender, handle)
    }
}
