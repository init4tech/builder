//! Flashbots Task receives simulated blocks from an upstream channel and
//! submits them to the Flashbots relay as bundles.
use crate::{
    config::{HostProvider, ZenithInstance},
    quincey::Quincey,
    tasks::{block::sim::SimResult, submit::SubmitPrep},
    utils,
};
use alloy::{
    consensus::SimpleCoder,
    eips::Encodable2718,
    primitives::{TxHash, U256},
    rpc::types::mev::{BundleItem, MevSendBundle, ProtocolVersion},
};
use init4_bin_base::deps::tracing::{debug, error};
use signet_constants::SignetSystemConstants;
use signet_types::SignRequest;
use signet_zenith::{Alloy2718Coder, Zenith::BlockHeader, ZenithBlock};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::Instrument;

/// Errors that the `FlashbotsTask` can encounter.
#[derive(Debug)]
pub enum FlashbotsError {
    /// Error during bundle preparation.
    PreparationError(String),
    /// Error during bundle simulation.
    SimulationError(String),
    /// Error during bundle submission.
    SubmissionError(String),
}

/// Handles construction, simulation, and submission of rollup blocks to the
/// Flashbots network.
#[derive(Debug)]
pub struct FlashbotsTask {
    /// Builder configuration for the task.
    pub config: crate::config::BuilderConfig,
    /// System constants for the rollup and host chain.
    pub constants: SignetSystemConstants,
    /// Quincey instance for block signing.
    pub quincey: Quincey,
    /// Zenith instance.
    pub zenith: ZenithInstance<HostProvider>,
    /// Channel for sending hashes of outbound transactions.
    pub outbound: mpsc::UnboundedSender<TxHash>,
    /// Determines if the BundleHelper contract should be used for block submission.
    /// NB: If bundle_helper is not used, instead the Zenith contract
    /// submitBlock function is used.
    bundle_helper: bool,
}

impl FlashbotsTask {
    /// Returns a new `FlashbotsTask` instance that receives `SimResult` types from the given
    /// channel and handles their preparation, submission to the Flashbots network.
    pub const fn new(
        config: crate::config::BuilderConfig,
        constants: SignetSystemConstants,
        quincey: Quincey,
        zenith: ZenithInstance<HostProvider>,
        outbound: mpsc::UnboundedSender<TxHash>,
        bundle_helper: bool,
    ) -> FlashbotsTask {
        Self { config, constants, quincey, zenith, outbound, bundle_helper }
    }

    /// Returns a reference to the inner `HostProvider`
    pub fn host_provider(&self) -> HostProvider {
        self.zenith.provider().clone()
    }

    /// Prepares a MEV bundle with the configured submit call
    pub async fn prepare(&self, sim_result: &SimResult) -> Result<MevSendBundle, FlashbotsError> {
        match self.bundle_helper {
            true => self.prepare_bundle_helper(sim_result).await,
            false => self.prepare_zenith(sim_result).await,
        }
    }

    /// Prepares a BundleHelper call containing the rollup block and corresponding fills into a MEV bundle.
    async fn prepare_bundle_helper(
        &self,
        sim_result: &SimResult,
    ) -> Result<MevSendBundle, FlashbotsError> {
        let mut bundle_body = Vec::new();

        let prep = SubmitPrep::new(
            &sim_result.block,
            self.host_provider(),
            self.quincey.clone(),
            self.config.clone(),
            self.constants.clone(),
        );

        let tx = prep.prep_transaction(&sim_result.sim_env.prev_header).await.map_err(|err| {
            error!(?err, "failed to prepare bumpable transaction");
            FlashbotsError::PreparationError(err.to_string())
        })?;

        let sendable = self.host_provider().fill(tx.req().clone()).await.map_err(|err| {
            error!(?err, "failed to fill transaction");
            FlashbotsError::PreparationError(err.to_string())
        })?;

        if let Some(envelope) = sendable.as_envelope() {
            bundle_body
                .push(BundleItem::Tx { tx: envelope.encoded_2718().into(), can_revert: true });
            debug!(tx_hash = ?envelope.hash(), "added filled transaction to bundle");
        }

        let bundle = MevSendBundle::new(0, Some(0), ProtocolVersion::V0_1, bundle_body);

        Ok(bundle)
    }

    /// Creates a Zenith `submitBlock` transaction and packages it into a MEV bundle from a given simulation result.
    async fn prepare_zenith(
        &self,
        sim_result: &SimResult,
    ) -> Result<MevSendBundle, FlashbotsError> {
        let target_block = sim_result.block.block_number();
        let host_block_number = self.constants.rollup_block_to_host_block_num(target_block);

        // Create Zenith block header
        let header = BlockHeader {
            rollupChainId: U256::from(self.constants.ru_chain_id()),
            hostBlockNumber: U256::from(host_block_number),
            gasLimit: U256::from(self.config.rollup_block_gas_limit),
            rewardAddress: self.config.builder_rewards_address,
            blockDataHash: *sim_result.block.contents_hash(),
        };

        // Create the raw Zenith block from the header
        let block =
            ZenithBlock::<Alloy2718Coder>::new(header, sim_result.block.transactions().to_vec());

        // Encode the Zenith block as a blob sidecar
        let sidecar = sim_result.block.encode_blob::<SimpleCoder>().build().map_err(|err| {
            error!(?err, "failed to encode sidecar for block");
            FlashbotsError::PreparationError(err.to_string())
        })?;

        // Sign the rollup block contents
        let signature = self
            .quincey
            .get_signature(&SignRequest {
                host_block_number: U256::from(host_block_number),
                host_chain_id: U256::from(self.constants.host_chain_id()),
                ru_chain_id: U256::from(self.constants.ru_chain_id()),
                gas_limit: U256::from(self.config.rollup_block_gas_limit),
                ru_reward_address: self.config.builder_rewards_address,
                contents: *sim_result.block.contents_hash(),
            })
            .await
            .map_err(|err| {
                error!(?err, "failed to get signature for block");
                FlashbotsError::PreparationError(err.to_string())
            })?;

        // Extract the v, r, and s values from the signature
        let (v, r, s) = utils::extract_signature_components(&signature.sig);

        // Create a MEV bundle and add the host fills to it
        let mut bundle_body = Vec::new();
        let host_fills = sim_result.block.host_fills();
        for fill in host_fills {
            // Create a host fill transaction with the rollup orders contract and sign it
            let tx_req = fill.to_fill_tx(self.constants.ru_orders());
            let sendable = self.host_provider().fill(tx_req).await.map_err(|err| {
                error!(?err, "failed to fill transaction");
                FlashbotsError::PreparationError(err.to_string())
            })?;

            // Add them to the MEV bundle
            if let Some(envelope) = sendable.as_envelope() {
                bundle_body
                    .push(BundleItem::Tx { tx: envelope.encoded_2718().into(), can_revert: true });
                debug!(tx_hash = ?envelope.hash(), "added filled transaction to MEV bundle");
            }
        }

        // Create the Zenith submit block call transaction and attach the sidecar to it
        let submit_block_call = self
            .zenith
            .submitBlock(header, v, r, s, block.encoded_txns().to_vec().into())
            .sidecar(sidecar);

        // Create the transaction request for the submit call
        let rollup_tx = submit_block_call.into_transaction_request();
        let signed = self.host_provider().fill(rollup_tx).await.map_err(|err| {
            error!(?err, "failed to sign rollup transaction");
            FlashbotsError::PreparationError(err.to_string())
        })?;

        // Sign the rollup block tx and add it to the MEV bundle as the last transaction.
        if let Some(envelope) = signed.as_envelope() {
            bundle_body
                .push(BundleItem::Tx { tx: envelope.encoded_2718().into(), can_revert: false });
            debug!(tx_hash = ?envelope.hash(), "added rollup transaction to MEV bundle");
        }

        // Create the MEV bundle and return it
        let bundle = MevSendBundle::new(
            target_block,
            Some(target_block),
            ProtocolVersion::V0_1,
            bundle_body,
        );

        Ok(bundle)
    }

    /// Task future that runs the Flashbots submission loop.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<SimResult>) {
        debug!("starting flashbots task");

        let Some(flashbots) = self.config.flashbots_provider().await else {
            error!("flashbots configuration missing - exiting flashbots task");
            return;
        };

        loop {
            // Wait for a sim result to come in
            let Some(sim_result) = inbound.recv().await else {
                debug!("upstream task gone - exiting flashbots task");
                break;
            };

            let span = sim_result.span();
            span_scoped!(span, debug!("simulation result received"));

            // Prepare a MEV bundle with the configured call type from the sim result
            let Ok(bundle) = self.prepare(&sim_result).instrument(span.clone()).await else {
                span_scoped!(span, debug!("bundle preparation failed"));
                continue;
            };

            // simulate the bundle against Flashbots then send the bundle
            if let Err(err) = flashbots.simulate_bundle(&bundle).instrument(span.clone()).await {
                span_scoped!(span, debug!(%err, "bundle simulation failed"));
                continue;
            }

            let _ = flashbots
                .send_bundle(&bundle)
                .instrument(span.clone())
                .await
                .inspect(|bundle_hash| {
                    span_scoped!(
                        span,
                        debug!(bunlde_hash = %bundle_hash.bundle_hash, "bundle sent to Flashbots")
                    );
                })
                .inspect_err(|err| {
                    span_scoped!(span, error!(?err, "failed to send bundle to Flashbots"));
                });
        }
    }

    /// Spawns the Flashbots task that handles incoming `SimResult`s.
    pub fn spawn(self) -> (mpsc::UnboundedSender<SimResult>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel::<SimResult>();
        let handle = tokio::spawn(self.task_future(inbound));
        (sender, handle)
    }
}
