use crate::{
    config::{HostProvider, ZenithInstance},
    quincey::Quincey,
    tasks::env::SimEnv,
    utils::extract_signature_components,
};
use alloy::{
    consensus::{Header, SimpleCoder, constants::GWEI_TO_WEI},
    eips::BlockNumberOrTag,
    network::{TransactionBuilder, TransactionBuilder4844},
    primitives::{Bytes, FixedBytes, TxHash, U256},
    providers::{Provider as _, SendableTx, WalletProvider},
    rpc::{json_rpc::ErrorPayload, types::eth::TransactionRequest},
    sol_types::{SolCall, SolError},
    transports::TransportError,
};
use eyre::bail;
use init4_bin_base::deps::{
    metrics::{counter, histogram},
    tracing::{Instrument, debug, debug_span, error, info, instrument, warn},
};
use signet_sim::BuiltBlock;
use signet_types::{SignRequest, SignResponse};
use signet_zenith::{
    BundleHelper::{self, BlockHeader, FillPermit2, submitCall},
    Zenith::{self, IncorrectHostBlock},
};
use std::time::{Instant, UNIX_EPOCH};
use tokio::{
    sync::mpsc::{self},
    task::JoinHandle,
};

use crate::tasks::block::sim::SimResult;

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

/// Represents the kind of revert that can occur during simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimRevertKind {
    /// Incorrect host block error
    IncorrectHostBlock,
    /// Bad signature error
    BadSignature,
    /// One rollup block per host block error
    OneRollupBlockPerHostBlock,
    /// Unknown error
    Unknown,
}

impl From<Option<Bytes>> for SimRevertKind {
    fn from(data: Option<Bytes>) -> Self {
        let Some(data) = data else {
            return Self::Unknown;
        };

        if data.starts_with(&IncorrectHostBlock::SELECTOR) {
            Self::IncorrectHostBlock
        } else if data.starts_with(&Zenith::BadSignature::SELECTOR) {
            Self::BadSignature
        } else if data.starts_with(&Zenith::OneRollupBlockPerHostBlock::SELECTOR) {
            Self::OneRollupBlockPerHostBlock
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone)]
/// Represents an error that occurs during simulation of a transaction.
pub struct SimErrorResp {
    /// The error payload containing the error code and message.
    pub err: ErrorPayload,
    /// The kind of revert that occurred (or unknown if not recognized).
    kind: SimRevertKind,
}

impl core::fmt::Display for SimErrorResp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "SimErrorResp {{ code: {}, message: {}, kind: {:?} }}",
            self.code(),
            self.message(),
            self.kind
        )
    }
}

impl From<ErrorPayload> for SimErrorResp {
    fn from(err: ErrorPayload) -> Self {
        Self::new(err)
    }
}

impl SimErrorResp {
    /// Creates a new `SimRevertError` with the specified kind and error
    /// payload.
    pub fn new(err: ErrorPayload) -> Self {
        let kind = err.as_revert_data().into();
        Self { err, kind }
    }

    /// True if the error is an incorrect host block.
    pub fn is_incorrect_host_block(&self) -> bool {
        self.as_revert_data()
            .map(|b| b.starts_with(&IncorrectHostBlock::SELECTOR))
            .unwrap_or_default()
    }

    /// Attempts to decode the error payload as an [`IncorrectHostBlock`].
    pub fn as_incorrect_host_block(&self) -> Option<IncorrectHostBlock> {
        self.as_revert_data().and_then(|data| IncorrectHostBlock::abi_decode(&data).ok())
    }

    /// True if the error is a [`Zenith::BadSignature`].
    pub fn is_bad_signature(&self) -> bool {
        self.as_revert_data()
            .map(|b| b.starts_with(&Zenith::BadSignature::SELECTOR))
            .unwrap_or_default()
    }

    /// Attempts to decode the error payload as a [`Zenith::BadSignature`].
    pub fn as_bad_signature(&self) -> Option<Zenith::BadSignature> {
        self.as_revert_data().and_then(|data| Zenith::BadSignature::abi_decode(&data).ok())
    }

    /// True if the error is a [`Zenith::OneRollupBlockPerHostBlock`].
    pub fn is_one_rollup_block_per_host_block(&self) -> bool {
        self.as_revert_data()
            .map(|b| b.starts_with(&Zenith::OneRollupBlockPerHostBlock::SELECTOR))
            .unwrap_or_default()
    }

    /// Attempts to decode the error payload as a
    /// [`Zenith::OneRollupBlockPerHostBlock`].
    pub fn as_one_rollup_block_per_host_block(&self) -> Option<Zenith::OneRollupBlockPerHostBlock> {
        self.as_revert_data()
            .and_then(|data| Zenith::OneRollupBlockPerHostBlock::abi_decode(&data).ok())
    }

    /// True if the error is an unknown revert.
    pub fn is_unknown(&self) -> bool {
        !self.is_incorrect_host_block()
            && !self.is_bad_signature()
            && !self.is_one_rollup_block_per_host_block()
    }

    /// Returns the revert data if available.
    pub fn as_revert_data(&self) -> Option<Bytes> {
        self.err.as_revert_data()
    }

    /// Returns the JSON-RPC error code.
    pub const fn code(&self) -> i64 {
        self.err.code
    }

    /// Returns the error message.
    pub fn message(&self) -> &str {
        &self.err.message
    }
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
        Ok(SignRequest {
            host_block_number: U256::from(self.next_host_block_height().await?),
            host_chain_id: U256::from(self.config.host_chain_id),
            ru_chain_id: U256::from(self.config.ru_chain_id),
            gas_limit: U256::from(self.config.rollup_block_gas_limit),
            ru_reward_address: self.config.builder_rewards_address,
            contents: *contents.contents_hash(),
        })
    }

    /// Encodes the sidecar and then builds the 4844 blob transaction from the provided header and signature values.
    fn build_blob_tx(
        &self,
        fills: Vec<FillPermit2>,
        header: BundleHelper::BlockHeader,
        v: u8,
        r: FixedBytes<32>,
        s: FixedBytes<32>,
        block: &BuiltBlock,
    ) -> eyre::Result<TransactionRequest> {
        let data = submitCall { fills, header, v, r, s }.abi_encode();

        let sidecar = block.encode_blob::<SimpleCoder>().build()?;

        Ok(TransactionRequest::default().with_blob_sidecar(sidecar).with_input(data))
    }

    /// Prepares and sends the EIP-4844 transaction with a sidecar encoded with a rollup block to the network.
    async fn submit_transaction(
        &self,
        retry_count: usize,
        resp: &SignResponse,
        block: &BuiltBlock,
        sim_env: &SimEnv,
    ) -> eyre::Result<ControlFlow> {
        let tx = self.prepare_tx(retry_count, resp, block, sim_env).await?;

        self.send_transaction(resp, tx).await
    }

    /// Prepares the transaction by extracting the signature components, creating the transaction
    /// request, and simulating the transaction with a call to the host provider.
    async fn prepare_tx(
        &self,
        retry_count: usize,
        resp: &SignResponse,
        block: &BuiltBlock,
        sim_env: &SimEnv,
    ) -> Result<TransactionRequest, eyre::Error> {
        // Create the transaction request with the signature values
        let tx: TransactionRequest = self.new_tx_request(retry_count, resp, block, sim_env).await?;

        // Simulate the transaction with a call to the host provider and report any errors
        if let Err(err) = self.sim_with_call(&tx).await {
            warn!(%err, "error in transaction simulation");
        }

        Ok(tx)
    }

    /// Simulates the transaction with a call to the host provider to check for reverts.
    async fn sim_with_call(&self, tx: &TransactionRequest) -> eyre::Result<()> {
        match self.provider().call(tx.clone()).block(BlockNumberOrTag::Pending.into()).await {
            Err(TransportError::ErrorResp(e)) => {
                let e = SimErrorResp::from(e);
                bail!(e)
            }
            Err(e) => bail!(e),
            _ => Ok(()),
        }
    }

    /// Creates a transaction request for the blob with the given header and signature values.
    async fn new_tx_request(
        &self,
        retry_count: usize,
        resp: &SignResponse,
        block: &BuiltBlock,
        sim_env: &SimEnv,
    ) -> Result<TransactionRequest, eyre::Error> {
        // manually retrieve nonce
        let nonce =
            self.provider().get_transaction_count(self.provider().default_signer_address()).await?;
        debug!(nonce, "assigned nonce");

        // Extract the signature components from the response
        let (v, r, s) = extract_signature_components(&resp.sig);

        let (max_fee_per_gas, max_priority_fee_per_gas, max_fee_per_blob_gas) =
            calculate_gas(retry_count, sim_env.host.clone());

        // Build the block header
        let header: BlockHeader = BlockHeader {
            hostBlockNumber: resp.req.host_block_number,
            rollupChainId: U256::from(self.config.ru_chain_id),
            gasLimit: resp.req.gas_limit,
            rewardAddress: resp.req.ru_reward_address,
            blockDataHash: *block.contents_hash(),
        };
        debug!(?header.hostBlockNumber, "built rollup block header");

        // Extract fills from the built block
        let fills = self.extract_fills(block);
        debug!(fill_count = fills.len(), "extracted fills from rollup block");

        // Create a blob transaction with the blob header and signature values and return it
        let tx = self
            .build_blob_tx(fills, header, v, r, s, block)?
            .with_to(self.config.builder_helper_address)
            .with_max_fee_per_gas(max_fee_per_gas)
            .with_max_priority_fee_per_gas(max_priority_fee_per_gas)
            .with_max_fee_per_blob_gas(max_fee_per_blob_gas)
            .with_nonce(nonce);

        Ok(tx)
    }

    /// Fills the transaction request with the provider and sends it to the network
    /// and any additionally configured broadcast providers.
    async fn send_transaction(
        &self,
        resp: &SignResponse,
        tx: TransactionRequest,
    ) -> Result<ControlFlow, eyre::Error> {
        // assign the nonce and fill the rest of the values
        let SendableTx::Envelope(tx) = self.provider().fill(tx).await? else {
            bail!("failed to fill transaction")
        };
        debug!(tx_hash = ?tx.hash(), host_block_number = %resp.req.host_block_number, "sending transaction to network");

        // send the tx via the primary host_provider
        let fut = spawn_provider_send!(self.provider(), &tx);

        // spawn send_tx futures on retry attempts for all additional broadcast host_providers
        for host_provider in self.config.connect_additional_broadcast() {
            spawn_provider_send!(&host_provider, &tx);
        }

        // send the in-progress transaction over the outbound_tx_channel
        if self.outbound_tx_channel.send(*tx.tx_hash()).is_err() {
            error!("receipts task gone");
        }

        if let Err(e) = fut.await? {
            // Detect and handle transaction underprice errors
            if matches!(e, TransportError::ErrorResp(ref err) if err.code == -32603) {
                debug!(tx_hash = ?tx.hash(), "underpriced transaction error - retrying tx with gas bump");
                return Ok(ControlFlow::Retry);
            }

            // Unknown error, log and skip
            error!(error = %e, "Primary tx broadcast failed");
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

    /// Handles the inbound block by constructing a signature request and submitting the transaction.
    #[instrument(skip_all)]
    async fn handle_inbound(
        &self,
        retry_count: usize,
        block: &BuiltBlock,
        sim_env: &SimEnv,
    ) -> eyre::Result<ControlFlow> {
        info!(retry_count, txns = block.tx_count(), "handling inbound block");
        let Ok(sig_request) = self.construct_sig_request(block).await.inspect_err(|e| {
            error!(error = %e, "error constructing signature request");
        }) else {
            return Ok(ControlFlow::Skip);
        };

        debug!(
            host_block_number = %sig_request.host_block_number,
            ru_chain_id = %sig_request.ru_chain_id,
            tx_count = block.tx_count(),
            "constructed signature request for host block"
        );

        let signed = self.quincey.get_signature(&sig_request).await?;

        self.submit_transaction(retry_count, &signed, block, sim_env).await
    }

    /// Handles the retry logic for the inbound block.
    async fn retrying_handle_inbound(
        &self,
        block: &BuiltBlock,
        sim_env: &SimEnv,
        retry_limit: usize,
    ) -> eyre::Result<ControlFlow> {
        let mut retries = 0;
        let building_start_time = Instant::now();

        let (current_slot, start, end) = self.calculate_slot_window();
        debug!(current_slot, start, end, "calculating target slot window");

        // Retry loop
        let result = loop {
            let span = debug_span!("SubmitTask::retrying_handle_inbound", retries);

            let inbound_result =
                match self.handle_inbound(retries, block, sim_env).instrument(span.clone()).await {
                    Ok(control_flow) => control_flow,
                    Err(err) => {
                        // Delay until next slot if we get a 403 error
                        if err.to_string().contains("403 Forbidden") {
                            let (slot_number, _, _) = self.calculate_slot_window();
                            debug!(slot_number, "403 detected - skipping slot");
                            return Ok(ControlFlow::Skip);
                        } else {
                            // Otherwise, log error and retry
                            error!(error = %err, "error handling inbound block");
                        }

                        ControlFlow::Retry
                    }
                };

            let guard = span.entered();

            match inbound_result {
                ControlFlow::Retry => {
                    retries += 1;
                    if retries > retry_limit {
                        counter!("builder.building_too_many_retries").increment(1);
                        debug!("retries exceeded - skipping block");
                        return Ok(ControlFlow::Skip);
                    }
                    drop(guard);
                    debug!(retries, start, end, "retrying block");
                    continue;
                }
                ControlFlow::Skip => {
                    counter!("builder.skipped_blocks").increment(1);
                    debug!(retries, "skipping block");
                    break inbound_result;
                }
                ControlFlow::Done => {
                    counter!("builder.submitted_successful_blocks").increment(1);
                    debug!(retries, "successfully submitted block");
                    break inbound_result;
                }
            }
        };

        // This is reached when `Done` or `Skip` is returned
        let elapsed = building_start_time.elapsed().as_millis() as f64;
        histogram!("builder.block_build_time").record(elapsed);
        info!(
            ?result,
            tx_count = block.tx_count(),
            block_number = block.block_number(),
            build_time = ?elapsed,
            "finished block building"
        );
        Ok(result)
    }

    /// Calculates and returns the slot number and its start and end timestamps for the current instant.
    fn calculate_slot_window(&self) -> (u64, u64, u64) {
        let now_ts = self.now();
        let current_slot = self.config.slot_calculator.calculate_slot(now_ts);
        let (start, end) = self.config.slot_calculator.calculate_slot_window(current_slot);
        (current_slot, start, end)
    }

    /// Returns the current timestamp in seconds since the UNIX epoch.
    fn now(&self) -> u64 {
        let now = std::time::SystemTime::now();
        now.duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    /// Returns the next host block height.
    async fn next_host_block_height(&self) -> eyre::Result<u64> {
        let block_num = self.provider().get_block_number().await?;
        Ok(block_num + 1)
    }

    // This function converts &[SignedFill] into [FillPermit2]
    fn extract_fills(&self, block: &BuiltBlock) -> Vec<FillPermit2> {
        block.host_fills().iter().map(FillPermit2::from).collect()
    }

    /// Task future for the submit task. This function runs the main loop of the task.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<SimResult>) {
        loop {
            // Wait to receive a new block
            let Some(sim_result) = inbound.recv().await else {
                debug!("upstream task gone - exiting submit task");
                break;
            };
            debug!(block_number = sim_result.block.block_number(), "submit channel received block");

            // Don't submit empty blocks
            if sim_result.block.is_empty() {
                debug!(
                    block_number = sim_result.block.block_number(),
                    "received empty block - skipping"
                );
                continue;
            }

            if let Err(e) =
                self.retrying_handle_inbound(&sim_result.block, &sim_result.env, 3).await
            {
                error!(error = %e, "error handling inbound block");
                continue;
            }
        }
    }

    /// Spawns the in progress block building task
    pub fn spawn(self) -> (mpsc::UnboundedSender<SimResult>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel::<SimResult>();
        let handle = tokio::spawn(self.task_future(inbound));
        (sender, handle)
    }
}

/// Calculates gas parameters based on the block environment and retry count.
fn calculate_gas(retry_count: usize, prev_header: Header) -> (u128, u128, u128) {
    let fallback_blob_basefee = 500;
    let fallback_basefee = 7;

    let base_fee_per_gas = match prev_header.base_fee_per_gas {
        Some(basefee) => basefee,
        None => fallback_basefee,
    };

    let parent_blob_basefee = prev_header.excess_blob_gas.unwrap_or(0) as u128;
    let blob_basefee = if parent_blob_basefee > 0 {
        // Use the parent blob base fee if available
        parent_blob_basefee
    } else {
        // Fallback to a default value if no blob base fee is set
        fallback_blob_basefee   
    };

    bump_gas_from_retries(retry_count, base_fee_per_gas, blob_basefee as u128)
}

/// Bumps the gas parameters based on the retry count, base fee, and blob base fee.
pub fn bump_gas_from_retries(
    retry_count: usize,
    basefee: u64,
    blob_basefee: u128,
) -> (u128, u128, u128) {
    const PRIORITY_FEE_BASE: u64 = 2 * GWEI_TO_WEI;
    const BASE_MULTIPLIER: u128 = 2;
    const BLOB_MULTIPLIER: u128 = 2;

    // Increase priority fee by 20% per retry
    let priority_fee =
        PRIORITY_FEE_BASE * (12u64.pow(retry_count as u32) / 10u64.pow(retry_count as u32));

    // Max fee includes basefee + priority + headroom (double basefee, etc.)
    let max_fee_per_gas = (basefee as u128) * BASE_MULTIPLIER + (priority_fee as u128);
    let max_fee_per_blob_gas = blob_basefee * BLOB_MULTIPLIER * (retry_count as u128 + 1);

    debug!(
        retry_count,
        max_fee_per_gas, priority_fee, max_fee_per_blob_gas, "calculated bumped gas parameters"
    );

    (max_fee_per_gas, priority_fee as u128, max_fee_per_blob_gas)
}
