use crate::{
    config::{HostProvider, ZenithInstance},
    quincey::Quincey,
    tasks::{
        block::sim::SimResult,
        submit::{Bumpable, SimErrorResp, SubmitPrep},
    },
    utils,
};
use alloy::{
    eips::BlockNumberOrTag,
    primitives::TxHash,
    providers::{Provider as _, SendableTx},
    rpc::types::eth::TransactionRequest,
    transports::TransportError,
};
use eyre::bail;
use init4_bin_base::deps::{
    metrics::{counter, histogram},
    tracing::{Instrument, debug, debug_span, error, info, warn},
};
use signet_constants::SignetSystemConstants;
use std::{ops::Range, time::Instant};
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

macro_rules! check_slot_still_valid {
    ($self:expr, $initial_slot:expr) => {
        if !$self.slot_still_valid($initial_slot) {
            debug!(
                current_slot =
                    $self.config.slot_calculator.current_slot().expect("host chain has started"),
                initial_slot = $initial_slot,
                "slot changed before submission - skipping block"
            );
            counter!("builder.slot_missed").increment(1);
            return Ok(ControlFlow::Skip);
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
    /// Constants
    pub constants: SignetSystemConstants,
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

    /// Fills the transaction request with the provider and sends it to the network
    /// and any additionally configured broadcast providers.
    async fn send_transaction(&self, tx: TransactionRequest) -> Result<ControlFlow, eyre::Error> {
        // assign the nonce and fill the rest of the values
        let SendableTx::Envelope(tx) = self.provider().fill(tx).await? else {
            bail!("failed to fill transaction")
        };
        debug!(tx_hash = ?tx.hash(), "sending transaction to network");

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

        if let Err(error) = fut.await? {
            // Detect and handle transaction underprice errors
            if matches!(error, TransportError::ErrorResp(ref err) if err.code == -32603) {
                debug!(tx_hash = ?tx.hash(), "underpriced transaction error - retrying tx with gas bump");
                return Ok(ControlFlow::Retry);
            }

            // Unknown error, log and skip
            error!(%error, "Primary tx broadcast failed");
            return Ok(ControlFlow::Skip);
        }

        info!(
            tx_hash = %tx.tx_hash(),
            "dispatched to network"
        );

        Ok(ControlFlow::Done)
    }

    /// Handles the retry logic for the inbound block.
    async fn retrying_send(
        &self,
        mut bumpable: Bumpable,
        retry_limit: usize,
    ) -> eyre::Result<ControlFlow> {
        let submitting_start_time = Instant::now();

        let (expected_slot, window) = self.get_expected_slot_and_window();

        let mut req = bumpable.req().clone();

        // Retry loop
        let result = loop {
            let span = debug_span!(
                parent: None,
                "SubmitTask::retrying_send",
                retries = bumpable.bump_count(),
                nonce = bumpable.req().nonce,
                expected_slot,
                start = window.start,
                end = window.end,
                unix_now = utils::now(),
            );

            // Check at the top of the loop if the slot is still valid. This
            // will prevent unnecessary retries if the slot has changed.
            check_slot_still_valid!(self, expected_slot);

            let inbound_result = self
                .send_transaction(req)
                .instrument(span.clone())
                .await
                .inspect_err(|e| error!(error = %e, "error sending transaction"))
                .unwrap_or(ControlFlow::Retry);

            let guard = span.entered();
            match inbound_result {
                ControlFlow::Retry => {
                    // bump the req
                    req = bumpable.bumped();
                    if bumpable.bump_count() > retry_limit {
                        counter!("builder.building_too_many_retries").increment(1);
                        debug!("retries exceeded - skipping block");
                        return Ok(ControlFlow::Skip);
                    }
                    drop(guard);
                    debug!(
                        retries = bumpable.bump_count(),
                        window.start, window.end, "retrying block"
                    );
                    continue;
                }
                ControlFlow::Skip => {
                    counter!("builder.skipped_blocks").increment(1);
                    debug!(retries = bumpable.bump_count(), "skipping block");
                    break inbound_result;
                }
                ControlFlow::Done => {
                    counter!("builder.submitted_successful_blocks").increment(1);
                    debug!(retries = bumpable.bump_count(), "successfully submitted block");
                    break inbound_result;
                }
            }
        };

        // This is reached when `Done` or `Skip` is returned
        let elapsed = submitting_start_time.elapsed().as_millis() as f64;
        histogram!("builder.submit_timer").record(elapsed);
        info!(
            ?result,
            build_time = ?elapsed,
            "finished block submitting"
        );
        Ok(result)
    }

    /// Gets the expected slot and the slot window for the current slot.
    fn get_expected_slot_and_window(&self) -> (usize, Range<u64>) {
        let expected_slot =
            self.config.slot_calculator.current_slot().expect("host chain has started");

        let window = self.config.slot_calculator.slot_window(expected_slot);

        (expected_slot, window)
    }

    /// Checks if a slot is still valid during submission retries.
    fn slot_still_valid(&self, initial_slot: usize) -> bool {
        initial_slot == self.config.slot_calculator.current_slot().expect("host chain has started")
    }

    /// Task future for the submit task. This function runs the main loop of the task.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<SimResult>) {
        loop {
            // Wait to receive a new block
            let Some(sim_result) = inbound.recv().await else {
                debug!("upstream task gone - exiting submit task");
                break;
            };
            let ru_block_number = sim_result.block.block_number();
            let host_block_number = self.constants.rollup_block_to_host_block_num(ru_block_number);

            let span = debug_span!(
                parent: None,
                "SubmitTask::task_future::transaction_prep",
                ru_block_number,
                host_block_number,
                block_tx_count = sim_result.block.tx_count(),
            );

            let guard = span.enter();

            debug!(ru_block_number, "submit channel received block");

            // Don't submit empty blocks
            if sim_result.block.is_empty() {
                debug!(ru_block_number, "received empty block - skipping");
                continue;
            }

            // drop guard before await
            drop(guard);

            // Fetch the previous host block, not the current host block which is currently being built
            let prev_host_block = host_block_number - 1;

            let Ok(Some(prev_host)) = self
                .provider()
                .get_block_by_number(prev_host_block.into())
                .into_future()
                .instrument(span.clone())
                .await
            else {
                span.in_scope(|| {
                    warn!(ru_block_number, host_block_number, "failed to get previous host block")
                });
                continue;
            };

            // Prep the span we'll use for the transaction submission
            let submission_span = debug_span!(
                parent: &span,
                "SubmitTask::task_future::transaction_submission",
                tx_count = sim_result.block.tx_count(),
                host_block_number,
                ru_block_number,
            );

            // Prepare the transaction request for submission
            let prep = SubmitPrep::new(
                &sim_result.block,
                self.provider().clone(),
                self.quincey.clone(),
                self.config.clone(),
                self.constants,
            );
            let bumpable = match prep
                .prep_transaction(&prev_host.header)
                .instrument(submission_span.clone())
                .await
            {
                Ok(bumpable) => bumpable,
                Err(error) => {
                    submission_span.in_scope(|| {
                        error!(%error, "failed to prepare transaction for submission");
                    });
                    continue;
                }
            };

            // Simulate the transaction to check for reverts
            if let Err(error) =
                self.sim_with_call(bumpable.req()).instrument(submission_span.clone()).await
            {
                submission_span.in_scope(|| {
                    error!(%error, "simulation failed for transaction");
                });
                continue;
            };

            // Now send the transaction
            if let Err(error) =
                self.retrying_send(bumpable, 3).instrument(submission_span.clone()).await
            {
                submission_span.in_scope(|| {
                    error!(%error, "error dispatching block to host chain");
                });
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
