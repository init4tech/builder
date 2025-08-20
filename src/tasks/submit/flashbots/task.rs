//! Flashbots Task receives simulated blocks from an upstream channel and
//! submits them to the Flashbots relay as bundles.
use core::error;

use crate::{
    config::{HostProvider, ZenithInstance},
    quincey::Quincey,
    tasks::{
        block::sim::{self, SimResult},
        submit::flashbots::FlashbotsProvider,
    },
    utils,
};
use alloy::{
    consensus::SimpleCoder,
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{TxHash, U256},
    providers::{Provider, SendableTx, fillers::TxFiller},
    rlp::{BytesMut, bytes},
    rpc::types::{
        TransactionRequest,
        mev::{BundleItem, MevSendBundle, ProtocolVersion},
    },
};
use init4_bin_base::deps::tracing::{debug, error};
use signet_constants::SignetSystemConstants;
use signet_types::SignRequest;
use signet_zenith::{
    Alloy2718Coder,
    Zenith::{BlockHeader, submitBlockCall},
    ZenithBlock,
};
use tokio::{sync::mpsc, task::JoinHandle};

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
}

impl FlashbotsTask {
    /// Returns a new `FlashbotsTask` instance that receives `SimResult` types from the given
    /// channel and handles their preparation, submission to the Flashbots network.
    pub fn new(
        config: crate::config::BuilderConfig,
        constants: SignetSystemConstants,
        quincey: Quincey,
        zenith: ZenithInstance<HostProvider>,
        outbound: mpsc::UnboundedSender<TxHash>,
    ) -> FlashbotsTask {
        Self { config, constants, quincey, zenith, outbound }
    }

    /// Returns a reference to the inner `HostProvider`
    pub fn host_provider(&self) -> HostProvider {
        self.zenith.provider().clone()
    }

    /// Prepares a bundle transaction from the simulation result.
    pub async fn prepare_bundle(
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
            blockDataHash: sim_result.block.contents_hash().clone(),
        };

        // Create the raw Zenith block
        let block =
            ZenithBlock::<Alloy2718Coder>::new(header, sim_result.block.transactions().to_vec());

        // Encode the Zenith block as a blob sidecar
        let sidecar = sim_result.block.encode_blob::<SimpleCoder>().build().map_err(|err| {
            error!(?err, "failed to encode sidecar for block");
            FlashbotsError::PreparationError(err.to_string())
        })?;

        // Sign the block
        let signature = self
            .quincey
            .get_signature(&SignRequest {
                host_block_number: U256::from(host_block_number),
                host_chain_id: U256::from(self.constants.host_chain_id()),
                ru_chain_id: U256::from(self.constants.ru_chain_id()),
                gas_limit: U256::from(self.config.rollup_block_gas_limit),
                ru_reward_address: self.config.builder_rewards_address,
                contents: sim_result.block.contents_hash().clone(),
            })
            .await
            .map_err(|err| {
                error!(?err, "failed to get signature for block");
                FlashbotsError::PreparationError(err.to_string())
            })?;

        // Extract v, r, and s values from the signature
        let (v, r, s) = utils::extract_signature_components(&signature.sig);

        // Create the Zenith submit block call transaction and attach the sidecar to it
        let submit_block_call = self
            .zenith
            .submitBlock(header, v, r, s, block.encoded_txns().to_vec().into())
            .sidecar(sidecar);

        // Create the MEV bundle from the target block and bundle body
        let mut bundle_body = Vec::new();

        // Add host fills to the MEV bundle
        let host_fills = sim_result.block.host_fills();
        for fill in host_fills {
            let tx = fill.to_fill_tx(self.constants.ru_orders());
            let filled_tx = self.zenith.provider().fill(tx.into()).await.map_err(|err| {
                error!(?err, "failed to fill transaction");
                FlashbotsError::PreparationError(err.to_string())
            })?;
        }

        // TODO: Then add the submit block tx
        // bundle_body.push(submit_block_call.into_transaction_request());

        let bundle = MevSendBundle::new(
            target_block,
            Some(target_block),
            ProtocolVersion::V0_1,
            bundle_body, // TODO: Fill with host_fills, then add the rollup transaction
        );

        Ok(bundle)
    }

    /// Task future that runs the Flashbots submission loop.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<SimResult>) {
        debug!("starting flashbots task");

        let zenith = self.config.connect_zenith(self.host_provider());

        let flashbots = FlashbotsProvider::new(
            self.config.flashbots_endpoint.clone(), // TODO: Handle proper Option checks here
            zenith,
            &self.config,
        );

        loop {
            let Some(sim_result) = inbound.recv().await else {
                debug!("upstream task gone - exiting flashbots task");
                break;
            };
            debug!(?sim_result.env.block_env.number, "simulation block received");

            // Prepare the Flashbots bundle from the SimResult, skipping on error.
            let bundle = match self.prepare_bundle(&sim_result).await {
                Ok(b) => b,
                Err(err) => {
                    error!(?err, "bundle preparation failed");
                    continue;
                }
            };

            // simulate then send the bundle
            let sim_bundle_result = flashbots.simulate_bundle(bundle.clone()).await;
            match sim_bundle_result {
                Ok(_) => {
                    debug!("bundle simulation successful, ready to send");
                    match flashbots.send_bundle(bundle).await {
                        Ok(bundle_hash) => {
                            debug!(?bundle_hash, "bundle successfully sent to Flashbots");
                        }
                        Err(err) => {
                            error!(?err, "failed to send bundle to Flashbots");
                            continue;
                        }
                    }
                }
                Err(err) => {
                    error!(?err, "bundle simulation failed");
                    continue;
                }
            }
        }
    }

    /// Spawns the Flashbots task that handles incoming `SimResult`s.
    pub fn spawn(self) -> (mpsc::UnboundedSender<SimResult>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel::<SimResult>();
        let handle = tokio::spawn(self.task_future(inbound));
        (sender, handle)
    }
}
