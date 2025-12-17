//! Flashbots Task receives simulated blocks from an upstream channel and
//! submits them to the Flashbots relay as bundles.
use crate::{
    config::{BuilderConfig, FlashbotsProvider, HostProvider, ZenithInstance},
    quincey::Quincey,
    tasks::{block::sim::SimResult, submit::SubmitPrep},
};
use alloy::{
    eips::Encodable2718,
    primitives::TxHash,
    providers::ext::MevApi,
    rpc::types::mev::{BundleItem, EthSendBundle},
};
use eyre::OptionExt;
use init4_bin_base::{deps::metrics::counter, utils::signer::LocalOrAws};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{Instrument, debug, debug_span, error};

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
}

impl FlashbotsTask {
    /// Returns a new `FlashbotsTask` instance that receives `SimResult` types from the given
    /// channel and handles their preparation, submission to the Flashbots network.
    pub async fn new(outbound: mpsc::UnboundedSender<TxHash>) -> eyre::Result<FlashbotsTask> {
        let config = crate::config();

        let (quincey, host_provider, flashbots, builder_key) = tokio::try_join!(
            config.connect_quincey(),
            config.connect_host_provider(),
            config.connect_flashbots(),
            config.connect_builder_signer()
        )?;

        let zenith = config.connect_zenith(host_provider);

        Ok(Self { config, quincey, zenith, flashbots, signer: builder_key, outbound })
    }

    /// Prepares a MEV bundle from a simulation result.
    ///
    /// This function serves as an entry point for bundle preparation and is left
    /// for forward compatibility when adding different bundle preparation methods.
    pub async fn prepare(&self, sim_result: &SimResult) -> eyre::Result<EthSendBundle> {
        // This function is left for forwards compatibility when we want to add
        // different bundle preparation methods in the future.
        self.prepare_bundle(sim_result).await
    }

    /// Prepares a MEV bundle containing the host transactions and the rollup block.
    ///
    /// This method orchestrates the bundle preparation by:
    /// 1. Preparing and signing the submission transaction
    /// 2. Tracking the transaction hash for monitoring
    /// 3. Encoding the transaction for bundle inclusion
    /// 4. Constructing the complete bundle body
    async fn prepare_bundle(&self, sim_result: &SimResult) -> eyre::Result<EthSendBundle> {
        // Prepare and sign the transaction
        let block_tx = self.prepare_signed_transaction(sim_result).await?;

        // Track the outbound transaction
        self.track_outbound_tx(&block_tx);

        // Encode the transaction
        let tx_bytes = block_tx.encoded_2718().into();

        // Build the bundle body with the block_tx bytes as the last transaction in the bundle.
        let bundle_body = self.build_bundle_body(sim_result, tx_bytes);

        let bundle_bz = bundle_body
            .into_iter()
            .map(|item| match item {
                BundleItem::Tx { tx, .. } => tx,
                _ => vec![].into(),
            })
            .collect();

        let bundle = EthSendBundle {
            block_number: sim_result.host_block_number(),
            txs: bundle_bz,
            ..Default::default()
        };
        debug!(txn_count = bundle.txs.len(), ?bundle.block_number, bundle_hash = ?bundle.bundle_hash(), "prepared eth send bundle");

        Ok(bundle)
    }

    /// Prepares and signs the submission transaction for the rollup block.
    ///
    /// Creates a `SubmitPrep` instance to build the transaction, then fills
    /// and signs it using the host provider.
    async fn prepare_signed_transaction(
        &self,
        sim_result: &SimResult,
    ) -> eyre::Result<alloy::consensus::TxEnvelope> {
        debug!(block_number = ?sim_result.block.block_number(), "preparing signed transaction for flashbots bundle");

        let prep = SubmitPrep::new(
            &sim_result.block,
            self.host_provider(),
            self.quincey.clone(),
            self.config.clone(),
        );

        let tx = prep.prep_transaction(sim_result.prev_host()).await?;
        let sendable = self.host_provider().fill(tx.into_request()).await?;

        sendable.as_envelope().ok_or_eyre("failed to get envelope from filled tx").cloned()
    }

    /// Tracks the outbound transaction hash and increments submission metrics.
    ///
    /// Sends the transaction hash to the outbound channel for monitoring.
    /// Logs a debug message if the channel is closed.
    fn track_outbound_tx(&self, envelope: &alloy::consensus::TxEnvelope) {
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
    ) -> Vec<BundleItem> {
        sim_result
            .block
            .host_transactions()
            .iter()
            .cloned()
            .chain(std::iter::once(tx_bytes))
            .map(|tx| BundleItem::Tx { tx, can_revert: false })
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

            let span = sim_result.sim_env.clone_span();

            // Don't submit empty blocks
            if sim_result.block.is_empty() {
                counter!("signet.builder.flashbots.empty_block").increment(1);
                span_debug!(span, "received empty block - skipping");
                continue;
            }
            span_debug!(span, "flashbots task received block");

            // Prepare a EthSendBundle with the configured call type from the sim result
            let result =
                self.prepare(&sim_result).instrument(span.clone()).await.inspect_err(|error| {
                    counter!("signet.builder.flashbots.bundle_prep_failures").increment(1);
                    span_debug!(span, %error, "bundle preparation failed");
                });

            let bundle = match result {
                Ok(bundle) => bundle,
                Err(_) => continue,
            };

            // Make a child span to cover submission, or use the current span
            // if debug is not enabled.
            let _guard = span.enter();
            let submit_span = debug_span!(
                parent: &span,
                "flashbots.submit",
            )
            .or_current();

            // Send the bundle to Flashbots, instrumenting the send future so
            // all events inside the async send are attributed to the submit
            // span.
            let flashbots = self.flashbots().to_owned();
            let signer = self.signer.clone();

            tokio::spawn(
                async move {
                    let response = flashbots
                        .send_bundle(bundle.clone())
                        .with_auth(signer.clone())
                        .into_future()
                        .await;

                    match response {
                        Ok(resp) => {
                            counter!("signet.builder.flashbots.bundles_submitted").increment(1);
                            debug!(
                                hash = resp.map(|r| r.bundle_hash.to_string()),
                                "Submitted MEV bundle to Flashbots, received OK response"
                            );
                        }
                        Err(err) => {
                            counter!("signet.builder.flashbots.submission_failures").increment(1);
                            error!(%err, "MEV bundle submission failed - error returned");
                        }
                    }
                }
                .instrument(submit_span.clone()),
            );
        }
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
