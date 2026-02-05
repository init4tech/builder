//! Flashbots Task receives simulated blocks from an upstream channel and
//! submits them to the Flashbots relay as bundles.
use crate::{
    config::{BuilderConfig, FlashbotsProvider, HostProvider, ZenithInstance},
    quincey::Quincey,
    tasks::{block::sim::SimResult, submit::SubmitPrep},
};
use alloy::{
    consensus::TxEnvelope,
    eips::{Encodable2718, eip7594::BlobTransactionSidecarEip7594},
    primitives::{Bytes, TxHash},
    providers::ext::MevApi,
    rpc::types::mev::EthSendBundle,
};
use init4_bin_base::{deps::metrics::counter, utils::signer::LocalOrAws};
use std::time::{Duration, Instant};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{Instrument, debug, debug_span, error, info, instrument, warn};

/// Handles preparation and submission of simulated rollup blocks to the
/// Flashbots relay as MEV bundles.
#[derive(Debug)]
pub struct FlashbotsTask {
    /// Builder configuration for the task.
    config: &'static BuilderConfig,
    /// Quincey instance for block signing.
    quincey: Quincey,
    /// Zenith instance.
    zenith: ZenithInstance<HostProvider>,
    /// Provides access to a Flashbots-compatible bundle API.
    flashbots: FlashbotsProvider,
    /// The key used to sign requests to the Flashbots relay.
    signer: LocalOrAws,
    /// Channel for sending hashes of outbound transactions.
    outbound: mpsc::UnboundedSender<TxHash>,
    /// Channel for sending sidecars to the Pylon task.
    pylon_sender: mpsc::UnboundedSender<(TxHash, BlobTransactionSidecarEip7594)>,
}

impl FlashbotsTask {
    /// Returns a new `FlashbotsTask` instance that receives `SimResult` types from the given
    /// channel and handles their preparation, submission to the Flashbots network.
    pub async fn new(
        outbound: mpsc::UnboundedSender<TxHash>,
        pylon_sender: mpsc::UnboundedSender<(TxHash, BlobTransactionSidecarEip7594)>,
    ) -> eyre::Result<FlashbotsTask> {
        let config = crate::config();

        let (quincey, host_provider, flashbots, builder_key) = tokio::try_join!(
            config.connect_quincey(),
            config.connect_host_provider(),
            config.connect_flashbots(),
            config.connect_builder_signer()
        )?;

        let zenith = config.connect_zenith(host_provider);

        Ok(Self { config, quincey, zenith, flashbots, signer: builder_key, outbound, pylon_sender })
    }

    /// Builds a MEV bundle from a signed transaction envelope and simulation result.
    fn build_bundle(&self, envelope: &TxEnvelope, sim_result: &SimResult) -> EthSendBundle {
        let tx_bytes: Bytes = envelope.encoded_2718().into();
        let txs = self.build_bundle_body(sim_result, tx_bytes);

        EthSendBundle { txs, block_number: sim_result.host_block_number(), ..Default::default() }
    }

    /// Prepares and signs the submission transaction for the rollup block.
    ///
    /// Creates a `SubmitPrep` instance to build the transaction, then fills
    /// and signs it using the host provider.
    ///
    /// Returns the signed transaction envelope and the sidecar (for forwarding to Pylon).
    #[instrument(skip_all, level = "debug")]
    async fn prepare_signed_transaction(
        &self,
        sim_result: &SimResult,
    ) -> eyre::Result<(TxEnvelope, alloy::eips::eip7594::BlobTransactionSidecarEip7594)> {
        let prep = SubmitPrep::new(
            &sim_result.block,
            self.host_provider(),
            self.quincey.clone(),
            self.config.clone(),
        );

        let (tx, sidecar) = prep.prep_transaction(sim_result.prev_host()).await?;

        let sendable = self
            .host_provider()
            .fill(tx.into_request())
            .instrument(tracing::debug_span!("fill_tx").or_current())
            .await?;

        let tx_envelope = sendable.try_into_envelope()?;
        debug!(tx_hash = ?tx_envelope.hash(), "prepared signed rollup block transaction envelope");

        Ok((tx_envelope, sidecar))
    }

    /// Tracks the outbound transaction hash and increments submission metrics.
    ///
    /// Sends the transaction hash to the outbound channel for monitoring.
    /// Logs a debug message if the channel is closed.
    fn track_outbound_tx(&self, envelope: &TxEnvelope) {
        counter!("signet.builder.flashbots.").increment(1);
        let hash = *envelope.tx_hash();
        if self.outbound.send(hash).is_err() {
            debug!("outbound channel closed, could not track tx hash");
        }
    }

    /// Constructs the MEV bundle body from host transactions and the submission transaction.
    ///
    /// Combines all host transactions from the rollup block with the prepared rollup block
    /// submission transaction, wrapping each as a non-revertible bundle item.
    ///
    /// The rollup block transaction is placed last in the bundle.
    fn build_bundle_body(
        &self,
        sim_result: &SimResult,
        tx_bytes: alloy::primitives::Bytes,
    ) -> Vec<Bytes> {
        sim_result
            .block
            .host_transactions()
            .iter()
            .map(|tx| tx.encoded_2718().into())
            .chain(std::iter::once(tx_bytes))
            .collect()
    }

    /// Main task loop that processes simulation results and submits bundles to Flashbots.
    ///
    /// Receives `SimResult`s from the inbound channel, prepares MEV bundles, and submits
    /// them to the Flashbots relay. Skips empty blocks and continues processing on errors.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<SimResult>) {
        debug!("starting flashbots task");

        loop {
            // Wait for a sim result to come in
            let Some(sim_result) = inbound.recv().await else {
                debug!("upstream task gone - exiting flashbots task");
                break;
            };

            let span = sim_result.clone_span();

            // Calculate the submission deadline for this block
            let deadline = self.calculate_submit_deadline();

            // Don't submit empty blocks
            if sim_result.block.is_empty() {
                counter!("signet.builder.flashbots.empty_block").increment(1);
                span_debug!(span, "received empty block - skipping");
                continue;
            }
            span_debug!(span, "flashbots task received block");

            // Prepare and sign the transaction
            let (envelope, sidecar) =
                match self.prepare_signed_transaction(&sim_result).instrument(span.clone()).await {
                    Ok(result) => result,
                    Err(error) => {
                        counter!("signet.builder.flashbots.bundle_prep_failures").increment(1);
                        span_debug!(span, %error, "bundle preparation failed");
                        continue;
                    }
                };

            // Extract tx_hash and track the outbound transaction
            let tx_hash = *envelope.tx_hash();
            self.track_outbound_tx(&envelope);

            // Build the bundle
            let bundle = self.build_bundle(&envelope, &sim_result);

            // Make a child span to cover submission, or use the current span
            // if debug is not enabled.
            let _guard = span.enter();
            let submit_span = debug_span!("flashbots.submit",).or_current();

            // Send the bundle to Flashbots, instrumenting the send future so
            // all events inside the async send are attributed to the submit
            // span. Only send sidecar to Pylon if Flashbots accepts.
            let flashbots = self.flashbots().to_owned();
            let signer = self.signer.clone();
            let pylon_sender = self.pylon_sender.clone();

            tokio::spawn(
                async move {
                    let response =
                        flashbots.send_bundle(bundle).with_auth(signer.clone()).into_future().await;

                    // Check if we met the submission deadline
                    let met_deadline = Instant::now() <= deadline;

                    match (response, met_deadline) {
                        (Ok(resp), true) => {
                            counter!("signet.builder.flashbots.bundles_submitted").increment(1);
                            counter!("signet.builder.flashbots.deadline_met").increment(1);
                            info!(
                                hash = resp.as_ref().map(|r| r.bundle_hash.to_string()),
                                "Submitted MEV bundle to Flashbots within deadline"
                            );

                            // Only send sidecar to Pylon after Flashbots accepts
                            if let Err(e) = pylon_sender.send((tx_hash, sidecar)) {
                                debug!("pylon channel closed: {}", e);
                            }
                        }
                        (Ok(resp), false) => {
                            counter!("signet.builder.flashbots.bundles_submitted").increment(1);
                            counter!("signet.builder.flashbots.deadline_missed").increment(1);
                            warn!(
                                hash = resp.as_ref().map(|r| r.bundle_hash.to_string()),
                                "Submitted MEV bundle to Flashbots AFTER deadline - submission may be too late"
                            );
                        }
                        (Err(err), true) => {
                            counter!("signet.builder.flashbots.submission_failures").increment(1);
                            error!(%err, "MEV bundle submission failed - error returned");
                        }
                        (Err(err), false) => {
                            counter!("signet.builder.flashbots.submission_failures").increment(1);
                            counter!("signet.builder.flashbots.deadline_missed").increment(1);
                            error!(%err, "MEV bundle submission failed AFTER deadline - error returned");
                        }
                    }
                }
                .instrument(submit_span),
            );
        }
    }

    /// Calculates the deadline for bundle submission.
    ///
    /// The deadline is calculated as the time remaining in the current slot,
    /// minus the configured submit deadline buffer. Submissions completing
    /// after this deadline will be logged as warnings.
    ///
    /// # Returns
    ///
    /// An `Instant` representing the submission deadline.
    fn calculate_submit_deadline(&self) -> Instant {
        let slot_calculator = &self.config.slot_calculator;

        // Get the current number of milliseconds into the slot.
        let timepoint_ms =
            slot_calculator.current_point_within_slot_ms().expect("host chain has started");

        let slot_duration = slot_calculator.slot_duration() * 1000; // convert to milliseconds
        let submit_buffer = self.config.submit_deadline_buffer;

        // To find the remaining slot time, subtract the timepoint from the slot duration.
        // Then subtract the submit deadline buffer to give us margin before slot ends.
        let remaining = slot_duration.saturating_sub(timepoint_ms).saturating_sub(submit_buffer);

        // The deadline is calculated by adding the remaining time to the current instant.
        let deadline = Instant::now() + Duration::from_millis(remaining);
        deadline.max(Instant::now())
    }

    /// Returns a clone of the host provider for transaction operations.
    fn host_provider(&self) -> HostProvider {
        self.zenith.provider().clone()
    }

    /// Returns a reference to the Flashbots provider.
    const fn flashbots(&self) -> &FlashbotsProvider {
        &self.flashbots
    }

    /// Spawns the Flashbots task in a new Tokio task.
    ///
    /// Returns a channel sender for submitting `SimResult`s and a join handle for the task.
    pub fn spawn(self) -> (mpsc::UnboundedSender<SimResult>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel::<SimResult>();
        let handle = tokio::spawn(self.task_future(inbound));
        (sender, handle)
    }
}
