use crate::{
    config::{HostProvider, ZenithInstance},
    quincey::Quincey,
    utils::extract_signature_components,
};
use alloy::{
    consensus::{SimpleCoder, constants::GWEI_TO_WEI},
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
use tokio::{sync::mpsc, task::JoinHandle};

/// Base maximum fee per gas to use as a starting point for retry bumps
pub const BASE_FEE_PER_GAS: u128 = 10 * GWEI_TO_WEI as u128;
/// Base max priority fee per gas to use as a starting point for retry bumps
pub const BASE_MAX_PRIORITY_FEE_PER_GAS: u128 = 2 * GWEI_TO_WEI as u128;
/// Base maximum fee per blob gas to use as a starting point for retry bumps
pub const BASE_MAX_FEE_PER_BLOB_GAS: u128 = GWEI_TO_WEI as u128;

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
        self.as_revert_data().and_then(|data| IncorrectHostBlock::abi_decode(&data, true).ok())
    }

    /// True if the error is a [`Zenith::BadSignature`].
    pub fn is_bad_signature(&self) -> bool {
        self.as_revert_data()
            .map(|b| b.starts_with(&Zenith::BadSignature::SELECTOR))
            .unwrap_or_default()
    }

    /// Attempts to decode the error payload as a [`Zenith::BadSignature`].
    pub fn as_bad_signature(&self) -> Option<Zenith::BadSignature> {
        self.as_revert_data().and_then(|data| Zenith::BadSignature::abi_decode(&data, true).ok())
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
            .and_then(|data| Zenith::OneRollupBlockPerHostBlock::abi_decode(&data, true).ok())
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

    /// Builds blob transaction and encodes the sidecar for it from the provided header and signature values
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
    ) -> eyre::Result<ControlFlow> {
        let tx = self.prepare_tx(retry_count, resp, block).await?;

        self.send_transaction(resp, tx).await
    }

    /// Prepares the transaction by extracting the signature components, creating the transaction
    /// request, and simulating the transaction with a call to the host provider.
    async fn prepare_tx(
        &self,
        retry_count: usize,
        resp: &SignResponse,
        block: &BuiltBlock,
    ) -> Result<TransactionRequest, eyre::Error> {
        // Create the transaction request with the signature values
        let tx: TransactionRequest = self.new_tx_request(retry_count, resp, block).await?;

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
    ) -> Result<TransactionRequest, eyre::Error> {
        // manually retrieve nonce
        let nonce =
            self.provider().get_transaction_count(self.provider().default_signer_address()).await?;
        debug!(nonce, "assigned nonce");

        // Extract the signature components from the response
        let (v, r, s) = extract_signature_components(&resp.sig);

        // Calculate gas limits based on retry attempts
        let (max_fee_per_gas, max_priority_fee_per_gas, max_fee_per_blob_gas) =
            calculate_gas_limits(
                retry_count,
                BASE_FEE_PER_GAS,
                BASE_MAX_PRIORITY_FEE_PER_GAS,
                BASE_MAX_FEE_PER_BLOB_GAS,
            );

        // Build the block header
        let header: BlockHeader = BlockHeader {
            hostBlockNumber: resp.req.host_block_number,
            rollupChainId: U256::from(self.config.ru_chain_id),
            gasLimit: resp.req.gas_limit,
            rewardAddress: resp.req.ru_reward_address,
            blockDataHash: *block.contents_hash(),
        };
        debug!(?header, "built block header");

        // Extract fills from the built block
        let fills = self.extract_fills(block);
        debug!(fill_count = fills.len(), "extracted fills");

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
            "constructed signature request for host block"
        );

        let signed = self.quincey.get_signature(&sig_request).await?;

        self.submit_transaction(retry_count, &signed, block).await
    }

    /// Handles the retry logic for the inbound block.
    async fn retrying_handle_inbound(
        &self,
        block: &BuiltBlock,
        retry_limit: usize,
    ) -> eyre::Result<ControlFlow> {
        let mut retries = 0;
        let building_start_time = Instant::now();
        let (current_slot, start, end) = self.calculate_slot_window();
        debug!(current_slot, start, end, "calculating target slot window");

        // Retry loop
        let result = loop {
            // Log the retry attempt
            let span = debug_span!("SubmitTask::retrying_handle_inbound", retries);

            let inbound_result =
                match self.handle_inbound(retries, block).instrument(span.clone()).await {
                    Ok(control_flow) => control_flow,
                    Err(err) => {
                        // Delay until next slot if we get a 403 error
                        if err.to_string().contains("403 Forbidden") {
                            let (slot_number, _, _) = self.calculate_slot_window();
                            debug!(slot_number, "403 detected - skipping slot");
                            return Ok(ControlFlow::Skip);
                        } else {
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
        histogram!("builder.block_build_time")
            .record(building_start_time.elapsed().as_millis() as f64);
        info!(?result, "finished block building");
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

    /// Task future for the submit task
    /// NB: This task assumes that the simulator will only send it blocks for
    /// slots that it's assigned.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<BuiltBlock>) {
        // Holds a reference to the last block we attempted to submit
        let mut last_block_attempted: u64 = 0;

        loop {
            // Wait to receive a new block
            let Some(block) = inbound.recv().await else {
                debug!("upstream task gone");
                break;
            };
            debug!(block_number = block.block_number(), ?block, "submit channel received block");

            // Only attempt each block number once
            if block.block_number() == last_block_attempted {
                debug!("block number is unchanged from last attempt - skipping");
                continue;
            }

            // This means we have encountered a new block, so reset the last block attempted
            last_block_attempted = block.block_number();
            debug!(last_block_attempted, "resetting last block attempted");

            if self.retrying_handle_inbound(&block, 3).await.is_err() {
                debug!("error handling inbound block");
                continue;
            };
        }
    }

    /// Spawns the in progress block building task
    pub fn spawn(self) -> (mpsc::UnboundedSender<BuiltBlock>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel();
        let handle = tokio::spawn(self.task_future(inbound));

        (sender, handle)
    }
}

// Returns gas parameters based on retry counts.
fn calculate_gas_limits(
    retry_count: usize,
    base_max_fee_per_gas: u128,
    base_max_priority_fee_per_gas: u128,
    base_max_fee_per_blob_gas: u128,
) -> (u128, u128, u128) {
    let bump_multiplier = 1150u128.pow(retry_count as u32); // 15% bump
    let blob_bump_multiplier = 2000u128.pow(retry_count as u32); // 100% bump (double each time) for blob gas
    let bump_divisor = 1000u128.pow(retry_count as u32);

    let max_fee_per_gas = base_max_fee_per_gas * bump_multiplier / bump_divisor;
    let max_priority_fee_per_gas = base_max_priority_fee_per_gas * bump_multiplier / bump_divisor;
    let max_fee_per_blob_gas = base_max_fee_per_blob_gas * blob_bump_multiplier / bump_divisor;

    debug!(
        retry_count,
        max_fee_per_gas,
        max_priority_fee_per_gas,
        max_fee_per_blob_gas,
        "calculated bumped gas parameters"
    );

    (max_fee_per_gas, max_priority_fee_per_gas, max_fee_per_blob_gas)
}
