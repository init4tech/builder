//! `block.rs` contains the Simulator and everything that wires it into an
//! actor that handles the simulation of a stream of bundles and transactions
//! and turns them into valid Pecorino blocks for network submission.
use crate::{
    config::{BuilderConfig, RuProvider},
    constants::{BASEFEE_DEFAULT, PECORINO_CHAIN_ID},
    tasks::bundler::Bundle,
};
use alloy::{
    consensus::TxEnvelope,
    eips::{BlockId, BlockNumberOrTag::Latest},
    network::Ethereum,
    primitives::{Address, B256, FixedBytes, U256},
    providers::Provider,
};
use chrono::{DateTime, Utc};
use eyre::Report;
use init4_bin_base::utils::calc::SlotCalculator;
use signet_sim::{BlockBuild, BuiltBlock, SimCache};
use signet_types::config::SignetSystemConstants;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    select,
    sync::mpsc::{self},
    task::JoinHandle,
    time::sleep,
};
use trevm::{
    Block,
    revm::{
        context::{BlockEnv, CfgEnv},
        context_interface::block::BlobExcessGasAndPrice,
        database::{AlloyDB, WrapDatabaseAsync},
        inspector::NoOpInspector,
        primitives::hardfork::SpecId::{self},
    },
};

/// Different error types that the Simulator handles
#[derive(Debug, Error)]
pub enum SimulatorError {
    /// Wraps errors encountered when interacting with the RPC
    #[error("RPC error: {0}")]
    Rpc(#[source] Report),
}

/// `Simulator` is responsible for periodically building blocks and submitting them for
/// signing and inclusion in the blockchain. It wraps a rollup provider and a slot
/// calculator with a builder configuration.
#[derive(Debug)]
pub struct Simulator {
    /// Configuration for the builder.
    pub config: BuilderConfig,
    /// A provider that cannot sign transactions, used for interacting with the rollup.
    pub ru_provider: RuProvider,
    /// The slot calculator for determining when to wake up and build blocks.
    pub slot_calculator: SlotCalculator,
}

type AlloyDatabaseProvider = WrapDatabaseAsync<AlloyDB<Ethereum, RuProvider>>;

impl Simulator {
    /// Creates a new `Simulator` instance.
    ///
    /// # Arguments
    ///
    /// - `config`: The configuration for the builder.
    /// - `ru_provider`: A provider for interacting with the rollup.
    /// - `slot_calculator`: A slot calculator for managing block timing.
    ///
    /// # Returns
    ///
    /// A new `Simulator` instance.
    pub fn new(
        config: &BuilderConfig,
        ru_provider: RuProvider,
        slot_calculator: SlotCalculator,
    ) -> Self {
        Self { config: config.clone(), ru_provider, slot_calculator }
    }

    /// Handles building a single block.
    ///
    /// # Arguments
    ///
    /// - `constants`: The system constants for the rollup.
    /// - `sim_items`: The simulation cache containing transactions and bundles.
    /// - `finish_by`: The deadline by which the block must be built.
    ///
    /// # Returns
    ///
    /// A `Result` containing the built block or an error.
    pub async fn handle_build(
        &self,
        constants: SignetSystemConstants,
        sim_items: SimCache,
        finish_by: Instant,
        block: PecorinoBlockEnv,
    ) -> Result<BuiltBlock, SimulatorError> {
        let db = self.create_db().await.unwrap();

        let block_build: BlockBuild<_, NoOpInspector> = BlockBuild::new(
            db,
            constants,
            PecorinoCfg {},
            block,
            finish_by,
            self.config.concurrency_limit,
            sim_items,
            self.config.rollup_block_gas_limit,
        );

        let block = block_build.build().await;
        tracing::debug!(block = ?block, "finished block simulation");

        Ok(block)
    }

    /// Spawns two tasks: one to handle incoming transactions and bundles,
    /// adding them to the simulation cache, and one to track the latest basefee.
    ///
    /// # Arguments
    ///
    /// - `tx_receiver`: A channel receiver for incoming transactions.
    /// - `bundle_receiver`: A channel receiver for incoming bundles.
    /// - `cache`: The simulation cache to store the received items.
    ///
    /// # Returns
    ///
    /// A `JoinHandle` for the basefee updater and a `JoinHandle` for the
    /// cache handler.
    pub fn spawn_cache_tasks(
        self: Arc<Self>,
        tx_receiver: mpsc::UnboundedReceiver<TxEnvelope>,
        bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
        cache: SimCache,
    ) -> (JoinHandle<()>, JoinHandle<()>) {
        tracing::debug!("starting up cache handler");

        let basefee_price = Arc::new(AtomicU64::new(0_u64));
        let basefee_reader = Arc::clone(&basefee_price);

        // Update the basefee on a per-block cadence
        let basefee_jh = tokio::spawn(async move { self.basefee_updater(basefee_price).await });

        // Update the sim cache whenever a transaction or bundle is received with respect to the basefee
        let cache_jh = tokio::spawn(async move {
            cache_updater(tx_receiver, bundle_receiver, cache, basefee_reader).await
        });

        (basefee_jh, cache_jh)
    }

    /// Periodically updates the shared basefee by querying the latest block.
    ///
    /// This function calculates the remaining time until the next slot,
    /// sleeps until that time, and then retrieves the latest basefee from the rollup provider.
    /// The updated basefee is stored in the provided `AtomicU64`.
    ///
    /// This function runs continuously.
    ///
    /// # Arguments
    ///
    /// - `price`: A shared `Arc<AtomicU64>` used to store the updated basefee value.
    async fn basefee_updater(self: Arc<Self>, price: Arc<AtomicU64>) {
        tracing::debug!("starting basefee updater");
        loop {
            // calculate start of next slot plus a small buffer
            let time_remaining = self.slot_calculator.slot_duration()
                - self.slot_calculator.current_timepoint_within_slot()
                + 1;
            tracing::debug!(time_remaining = ?time_remaining, "basefee updater sleeping until next slot");

            // wait until that point in time
            sleep(Duration::from_secs(time_remaining)).await;

            // update the basefee with that price
            self.check_basefee(&price).await;
        }
    }

    /// Queries the latest block from the rollup provider and updates the shared
    /// basefee value if a block is found.
    ///
    /// This function retrieves the latest block using the provider, extracts the
    /// `base_fee_per_gas` field from the block header (defaulting to zero if missing),
    /// and updates the shared `AtomicU64` price tracker. If no block is available,
    /// it logs a message without updating the price.
    ///
    /// # Arguments
    ///
    /// - `price`: A shared `Arc<AtomicU64>` used to store the updated basefee.
    async fn check_basefee(&self, price: &Arc<AtomicU64>) {
        let resp = self.ru_provider.get_block_by_number(Latest).await.inspect_err(|e| {
            tracing::error!(error = %e, "RPC error during basefee update");
        });

        if let Ok(Some(block)) = resp {
            let basefee = block.header.base_fee_per_gas.unwrap_or(0);
            price.store(basefee, Ordering::Relaxed);
            tracing::debug!(basefee = basefee, "basefee updated");
        } else {
            tracing::warn!("get basefee failed - an error likely occurred");
        }
    }

    /// Spawns the simulator task, which handles the setup and sets the deadline
    /// for the each round of simulation.
    ///
    /// # Arguments
    ///
    /// - `constants`: The system constants for the rollup.
    /// - `cache`: The simulation cache containing transactions and bundles.
    /// - `submit_sender`: A channel sender for submitting built blocks.
    ///
    /// # Returns
    ///
    /// A `JoinHandle` for the spawned task.
    pub fn spawn_simulator_task(
        self: Arc<Self>,
        constants: SignetSystemConstants,
        cache: SimCache,
        submit_sender: mpsc::UnboundedSender<BuiltBlock>,
    ) -> JoinHandle<()> {
        tracing::debug!("starting builder task");

        tokio::spawn(async move { self.run_simulator(constants, cache, submit_sender).await })
    }

    /// Continuously runs the block simulation and submission loop.
    ///
    /// This function clones the simulation cache, calculates a deadline for block building,
    /// attempts to build a block using the latest cache and constants, and submits the built
    /// block through the provided channel. If an error occurs during block building or submission,
    /// it logs the error and continues the loop.
    ///
    /// This function runs indefinitely and never returns.
    ///
    /// # Arguments
    ///
    /// - `constants`: The system constants for the rollup.
    /// - `cache`: The simulation cache containing transactions and bundles.
    /// - `submit_sender`: A channel sender used to submit built blocks.
    async fn run_simulator(
        self: Arc<Self>,
        constants: SignetSystemConstants,
        cache: SimCache,
        submit_sender: mpsc::UnboundedSender<BuiltBlock>,
    ) {
        loop {
            let sim_cache = cache.clone();
            let finish_by = self.calculate_deadline();

            let block_env = match self.next_block_env(finish_by).await {
                Ok(block) => block,
                Err(err) => {
                    tracing::error!(err = %err, "failed to configure next block");
                    break;
                }
            };
            tracing::info!(block_env = ?block_env, "created block");

            match self.handle_build(constants, sim_cache, finish_by, block_env).await {
                Ok(block) => {
                    tracing::debug!(block = ?block, "built block");
                    let _ = submit_sender.send(block);
                }
                Err(e) => {
                    tracing::error!(err = %e, "failed to build block");
                    continue;
                }
            }
        }
    }

    /// Calculates the deadline for the current block simulation.
    ///
    /// # Returns
    ///
    /// An `Instant` representing the simulation deadline, as calculated by determining
    /// the time left in the current slot and adding that to the current timestamp in UNIX seconds.
    pub fn calculate_deadline(&self) -> Instant {
        // Calculate the current timestamp in seconds since the UNIX epoch
        let now = SystemTime::now();
        let unix_seconds = now.duration_since(UNIX_EPOCH).expect("Time went backwards").as_secs();
        // Calculate the time remaining in the current slot
        let remaining = self.slot_calculator.calculate_timepoint_within_slot(unix_seconds);
        //  Deadline is equal to the start of the next slot plus the time remaining in this slot
        Instant::now() + Duration::from_secs(remaining)
    }

    /// Creates an `AlloyDB` instance from the rollup provider.
    ///
    /// # Returns
    ///
    /// An `Option` containing the wrapped database or `None` if an error occurs.
    async fn create_db(&self) -> Option<AlloyDatabaseProvider> {
        // Fetch latest block number
        let latest = match self.ru_provider.get_block_number().await {
            Ok(block_number) => block_number,
            Err(e) => {
                tracing::error!(error = %e, "failed to get latest block number");
                return None;
            }
        };

        // Make an AlloyDB instance from the rollup provider with that latest block number
        let alloy_db: AlloyDB<Ethereum, RuProvider> =
            AlloyDB::new(self.ru_provider.clone(), BlockId::from(latest));

        // Wrap the AlloyDB instance in a WrapDatabaseAsync and return it.
        // This is safe to unwrap because the main function sets the proper runtime settings.
        //
        // See: https://docs.rs/tokio/latest/tokio/attr.main.html
        let wrapped_db: AlloyDatabaseProvider = WrapDatabaseAsync::new(alloy_db).unwrap();
        Some(wrapped_db)
    }

    /// Prepares the next block environment.
    ///
    /// Prepares the next block environment to load into the simulator by fetching the latest block number,
    /// assigning the correct next block number, checking the basefee, and setting the timestamp,
    /// reward address, and gas configuration for the block environment based on builder configuration.
    ///
    /// # Arguments
    ///
    /// - finish_by: The deadline at which block simulation will end.
    async fn next_block_env(&self, finish_by: Instant) -> Result<PecorinoBlockEnv, SimulatorError> {
        let remaining = finish_by.duration_since(Instant::now());
        let finish_time = SystemTime::now() + remaining;
        let deadline: DateTime<Utc> = finish_time.into();
        tracing::debug!(deadline = %deadline, "preparing block env");

        // Fetch the latest block number and increment it by 1
        let latest_block_number = match self.ru_provider.get_block_number().await {
            Ok(num) => num,
            Err(err) => {
                tracing::error!(error = %err, "RPC error during block build");
                return Err(SimulatorError::Rpc(Report::new(err)));
            }
        };
        tracing::debug!(next_block_num = latest_block_number + 1, "preparing block env");

        // Fetch the basefee from previous block to calculate gas for this block
        let basefee = match self.get_basefee().await? {
            Some(basefee) => basefee,
            None => {
                tracing::warn!("get basefee failed - RPC error likely occurred");
                BASEFEE_DEFAULT
            }
        };
        tracing::debug!(basefee = basefee, "setting basefee");

        // Craft the Block environment to pass to the simulator
        let block_env = PecorinoBlockEnv::new(
            self.config.clone(),
            latest_block_number + 1,
            deadline.timestamp() as u64,
            basefee,
        );
        tracing::debug!(block_env = ?block_env, "prepared block env");

        Ok(block_env)
    }

    /// Returns the basefee of the latest block.
    ///
    /// # Returns
    ///
    /// The basefee of the previous (latest) block if the request was successful,
    /// or a sane default if the RPC failed.
    async fn get_basefee(&self) -> Result<Option<u64>, SimulatorError> {
        match self.ru_provider.get_block_by_number(Latest).await {
            Ok(maybe_block) => match maybe_block {
                Some(block) => {
                    tracing::debug!(basefee = ?block.header.base_fee_per_gas, "basefee found");
                    Ok(block.header.base_fee_per_gas)
                }
                None => Ok(None),
            },
            Err(err) => Err(SimulatorError::Rpc(err.into())),
        }
    }
}

/// Continuously updates the simulation cache with incoming transactions and bundles.
///
/// This function listens for new transactions and bundles on their respective
/// channels and adds them to the simulation cache using the latest observed basefee.
///
/// # Arguments
///
/// - `tx_receiver`: A receiver channel for incoming Ethereum transactions.
/// - `bundle_receiver`: A receiver channel for incoming transaction bundles.
/// - `cache`: The simulation cache used to store transactions and bundles.
/// - `price_reader`: An `Arc<AtomicU64>` providing the latest basefee for simulation pricing.
async fn cache_updater(
    mut tx_receiver: mpsc::UnboundedReceiver<
        alloy::consensus::EthereumTxEnvelope<alloy::consensus::TxEip4844Variant>,
    >,
    mut bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
    cache: SimCache,
    price_reader: Arc<AtomicU64>,
) -> ! {
    loop {
        let p = price_reader.load(Ordering::Relaxed);
        select! {
            maybe_tx = tx_receiver.recv() => {
                if let Some(tx) = maybe_tx {
                    tracing::debug!(tx = ?tx.hash(), "received transaction");
                    cache.add_item(tx, p);
                }
            }
            maybe_bundle = bundle_receiver.recv() => {
                if let Some(bundle) = maybe_bundle {
                    tracing::debug!(bundle = ?bundle.id, "received bundle");
                    cache.add_item(bundle.bundle, p);
                }
            }
        }
    }
}

/// PecorinoCfg holds network-level configuration values.
#[derive(Debug, Clone, Copy)]
pub struct PecorinoCfg {}

impl trevm::Cfg for PecorinoCfg {
    /// Fills the configuration environment with Pecorino-specific values.
    ///
    /// # Arguments
    ///
    /// - `cfg_env`: The configuration environment to be filled.
    fn fill_cfg_env(&self, cfg_env: &mut CfgEnv) {
        let CfgEnv { chain_id, spec, .. } = cfg_env;

        *chain_id = PECORINO_CHAIN_ID;
        *spec = SpecId::default();
    }
}

/// PecorinoBlockEnv holds block-level configurations for Pecorino blocks.
#[derive(Debug, Clone, Copy)]
pub struct PecorinoBlockEnv {
    /// The block number for this block.
    pub number: u64,
    /// The address the block reward should be sent to.
    pub beneficiary: Address,
    /// Timestamp for the block.
    pub timestamp: u64,
    /// The gas limit for this block environment.
    pub gas_limit: u64,
    /// The basefee to use for calculating gas usage.
    pub basefee: u64,
    /// The prevrandao to use for this block.
    pub prevrandao: Option<FixedBytes<32>>,
}

/// Implements [`trevm::Block`] for the Pecorino block.
impl Block for PecorinoBlockEnv {
    /// Fills the block environment with the Pecorino specific values
    fn fill_block_env(&self, block_env: &mut trevm::revm::context::BlockEnv) {
        // Destructure the fields off of the block_env and modify them
        let BlockEnv {
            number,
            beneficiary,
            timestamp,
            gas_limit,
            basefee,
            difficulty,
            prevrandao,
            blob_excess_gas_and_price,
        } = block_env;
        *number = self.number;
        *beneficiary = self.beneficiary;
        *timestamp = self.timestamp;
        *gas_limit = self.gas_limit;
        *basefee = self.basefee;
        *prevrandao = self.prevrandao;

        // NB: The following fields are set to sane defaults because they
        // are not supported by the rollup
        *difficulty = U256::ZERO;
        *blob_excess_gas_and_price =
            Some(BlobExcessGasAndPrice { excess_blob_gas: 0, blob_gasprice: 0 });
    }
}

impl PecorinoBlockEnv {
    /// Returns a new PecorinoBlockEnv with the specified values.
    ///
    /// # Arguments
    ///
    /// - config: The BuilderConfig for the builder.
    /// - number: The block number of this block, usually the latest block number plus 1,
    ///   unless simulating blocks in the past.
    /// - timestamp: The timestamp of the block, typically set to the deadline of the
    ///   block building task.
    fn new(config: BuilderConfig, number: u64, timestamp: u64, basefee: u64) -> Self {
        PecorinoBlockEnv {
            number,
            beneficiary: config.builder_rewards_address,
            timestamp,
            gas_limit: config.rollup_block_gas_limit,
            basefee,
            prevrandao: Some(B256::random()),
        }
    }
}
