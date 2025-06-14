//! `block.rs` contains the Simulator and everything that wires it into an
//! actor that handles the simulation of a stream of bundles and transactions
//! and turns them into valid Pecorino blocks for network submission.
use crate::config::{BuilderConfig, RuProvider};
use alloy::{eips::BlockId, network::Ethereum, providers::Provider};
use init4_bin_base::{
    deps::tracing::{debug, error},
    utils::calc::SlotCalculator,
};
use signet_sim::{BlockBuild, BuiltBlock, SimCache};
use signet_types::constants::SignetSystemConstants;
use std::time::{Duration, Instant};
use tokio::{
    sync::{
        mpsc::{self},
        watch,
    },
    task::JoinHandle,
};
use trevm::revm::{
    context::BlockEnv,
    database::{AlloyDB, WrapDatabaseAsync},
    inspector::NoOpInspector,
};

type AlloyDatabaseProvider = WrapDatabaseAsync<AlloyDB<Ethereum, RuProvider>>;

/// `Simulator` is responsible for periodically building blocks and submitting them for
/// signing and inclusion in the blockchain. It wraps a rollup provider and a slot
/// calculator with a builder configuration.
#[derive(Debug)]
pub struct Simulator {
    /// Configuration for the builder.
    pub config: BuilderConfig,
    /// A provider that cannot sign transactions, used for interacting with the rollup.
    pub ru_provider: RuProvider,

    /// The block configuration environment on which to simulate
    pub block_env: watch::Receiver<Option<BlockEnv>>,
}

impl Simulator {
    /// Creates a new `Simulator` instance.
    ///
    /// # Arguments
    ///
    /// - `config`: The configuration for the builder.
    /// - `ru_provider`: A provider for interacting with the rollup.
    ///
    /// # Returns
    ///
    /// A new `Simulator` instance.
    pub fn new(
        config: &BuilderConfig,
        ru_provider: RuProvider,
        block_env: watch::Receiver<Option<BlockEnv>>,
    ) -> Self {
        Self { config: config.clone(), ru_provider, block_env }
    }

    /// Get the slot calculator.
    pub const fn slot_calculator(&self) -> &SlotCalculator {
        &self.config.slot_calculator
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
        block: BlockEnv,
    ) -> eyre::Result<BuiltBlock> {
        let db = self.create_db().await.unwrap();
        let block_build: BlockBuild<_, NoOpInspector> = BlockBuild::new(
            db,
            constants,
            self.config.cfg_env(),
            block,
            finish_by,
            self.config.concurrency_limit,
            sim_items,
            self.config.rollup_block_gas_limit,
        );

        let built_block = block_build.build().await;
        debug!(block_number = ?built_block.block_number(), "finished building block");

        Ok(built_block)
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
        self,
        constants: SignetSystemConstants,
        cache: SimCache,
        submit_sender: mpsc::UnboundedSender<BuiltBlock>,
    ) -> JoinHandle<()> {
        debug!("starting simulator task");

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
        mut self,
        constants: SignetSystemConstants,
        cache: SimCache,
        submit_sender: mpsc::UnboundedSender<BuiltBlock>,
    ) {
        loop {
            let sim_cache = cache.clone();
            let finish_by = self.calculate_deadline();

            // Wait for the block environment to be set
            if self.block_env.changed().await.is_err() {
                error!("block_env channel closed");
                return;
            }

            // If no env, skip this run
            let Some(block_env) = self.block_env.borrow_and_update().clone() else { return };
            debug!(block_env = ?block_env, "building on block env");

            match self.handle_build(constants, sim_cache, finish_by, block_env).await {
                Ok(block) => {
                    debug!(block = ?block, "built block");
                    let _ = submit_sender.send(block);
                }
                Err(e) => {
                    error!(err = %e, "failed to build block");
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
        // Get the current timepoint within the slot.
        let timepoint = self.slot_calculator().current_timepoint_within_slot();

        // We have the timepoint in seconds into the slot. To find out what's
        // remaining, we need to subtract it from the slot duration
        let remaining = self.slot_calculator().slot_duration() - timepoint;

        // We add a 1500 ms buffer to account for sequencer stopping signing.

        let candidate =
            Instant::now() + Duration::from_secs(remaining) - Duration::from_millis(1500);

        candidate.max(Instant::now())
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
                error!(error = %e, "failed to get latest block number");
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
}
