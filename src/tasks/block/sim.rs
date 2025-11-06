//! `block.rs` contains the Simulator and everything that wires it into an
//! actor that handles the simulation of a stream of bundles and transactions
//! and turns them into valid Pecorino blocks for network submission.
use crate::{
    config::{BuilderConfig, HostProvider, RuProvider},
    tasks::env::SimEnv,
};
use alloy::{eips::BlockId, network::Ethereum, primitives::BlockNumber};
use init4_bin_base::{
    deps::metrics::{counter, histogram},
    utils::calc::SlotCalculator,
};
use signet_sim::{BlockBuild, BuiltBlock, HostEnv, RollupEnv, SimCache};
use signet_types::constants::SignetSystemConstants;
use std::time::{Duration, Instant};
use tokio::{
    sync::{
        mpsc::{self},
        watch,
    },
    task::JoinHandle,
};
use tracing::{Instrument, Span, debug, instrument};
use trevm::{
    Block,
    revm::{
        context::BlockEnv,
        database::{AlloyDB, WrapDatabaseAsync},
        inspector::NoOpInspector,
    },
};

type HostAlloyDatabaseProvider = WrapDatabaseAsync<AlloyDB<Ethereum, HostProvider>>;
type RollupAlloyDatabaseProvider = WrapDatabaseAsync<AlloyDB<Ethereum, RuProvider>>;

/// `Simulator` is responsible for periodically building blocks and submitting them for
/// signing and inclusion in the blockchain. It wraps a rollup provider and a slot
/// calculator with a builder configuration.
#[derive(Debug)]
pub struct Simulator {
    /// Configuration for the builder.
    pub config: BuilderConfig,
    /// Host Provider to interact with the host chain.
    pub host_provider: HostProvider,
    /// A provider that cannot sign transactions, used for interacting with the rollup.
    pub ru_provider: RuProvider,
    /// The block configuration environment on which to simulate
    pub sim_env: watch::Receiver<Option<SimEnv>>,
}

/// SimResult bundles a BuiltBlock to the BlockEnv it was simulated against.
#[derive(Debug, Clone)]
pub struct SimResult {
    /// The block built with the successfully simulated transactions
    pub block: BuiltBlock,
    /// The block environment the transactions were simulated against.
    pub sim_env: SimEnv,
}

impl SimResult {
    /// Returns the block number of the built block.
    pub const fn block_number(&self) -> u64 {
        self.block.block_number()
    }

    /// Returns the host block number for the built block.
    pub const fn host_block_number(&self) -> u64 {
        self.sim_env.host_block_number()
    }

    /// Returns a reference to the tracing span associated with this simulation
    /// result.
    pub const fn span(&self) -> &Span {
        self.sim_env.span()
    }

    /// Clones the span for use in other tasks.
    pub fn clone_span(&self) -> Span {
        self.sim_env.clone_span()
    }
}

impl Simulator {
    /// Creates a new `Simulator` instance.
    ///
    /// # Arguments
    ///
    /// - `config`: The configuration for the builder.
    /// - `ru_provider`: A provider for interacting with the rollup.
    /// - `block_env`: A receiver for the block environment to simulate against.
    ///
    /// # Returns
    ///
    /// A new `Simulator` instance.
    pub fn new(
        config: &BuilderConfig,
        host_provider: HostProvider,
        ru_provider: RuProvider,
        sim_env: watch::Receiver<Option<SimEnv>>,
    ) -> Self {
        Self { config: config.clone(), host_provider, ru_provider, sim_env }
    }

    /// Get the slot calculator.
    pub const fn slot_calculator(&self) -> &SlotCalculator {
        &self.config.slot_calculator
    }

    /// Handles building a single block.
    ///
    /// Builds a block in the block environment with items from the simulation cache
    /// against the database state. When the `finish_by` deadline is reached, it
    /// stops simulating and returns the block.
    ///
    /// # Arguments
    ///
    /// - `constants`: The system constants for the rollup.
    /// - `sim_items`: The simulation cache containing transactions and bundles.
    /// - `finish_by`: The deadline by which the block must be built.
    /// - `block_env`: The block environment to simulate against.
    ///
    /// # Returns
    ///
    /// A `Result` containing the built block or an error.
    #[instrument(skip_all, fields(
        tx_count = sim_items.len(),
        millis_to_deadline = finish_by.duration_since(Instant::now()).as_millis()
    ))]
    pub async fn handle_build(
        &self,
        constants: SignetSystemConstants,
        sim_items: SimCache,
        finish_by: Instant,
        sim_env: SimEnv,
    ) -> eyre::Result<BuiltBlock> {
        let concurrency_limit = self.config.concurrency_limit();

        let (rollup_env, host_env) = self.create_envs(constants, &sim_env).await;

        let max_host_gas = self.config.max_host_gas(sim_env.prev_host.gas_limit);

        let block_build = BlockBuild::new(
            rollup_env,
            host_env,
            finish_by,
            concurrency_limit,
            sim_items,
            self.config.rollup_block_gas_limit,
            max_host_gas,
        );

        let built_block = block_build.build().in_current_span().await;
        debug!(
            tx_count = built_block.tx_count(),
            block_number = built_block.block_number(),
            "block simulation completed",
        );
        counter!("signet.builder.built_blocks").increment(1);
        histogram!("signet.builder.built_blocks.tx_count").record(built_block.tx_count() as u32);

        Ok(built_block)
    }

    // Helper to create rollup + host envs from the sim env.
    async fn create_envs(
        &self,
        constants: SignetSystemConstants,
        sim_env: &SimEnv,
    ) -> (
        RollupEnv<RollupAlloyDatabaseProvider, NoOpInspector>,
        HostEnv<HostAlloyDatabaseProvider, NoOpInspector>,
    ) {
        // Host DB and Env
        let host_block_number = BlockNumber::from(sim_env.prev_host.number);
        let host_db = self.create_host_db(host_block_number).await;
        let mut host_block_env = BlockEnv::default();
        sim_env.prev_host.fill_block_env(&mut host_block_env);

        let host_env = HostEnv::<HostAlloyDatabaseProvider, NoOpInspector>::new(
            host_db,
            constants.clone(),
            &self.config.host_cfg_env(),
            &host_block_env,
        );

        // Rollup DB and Env
        let rollup_block_number = BlockNumber::from(sim_env.prev_header.number);
        let rollup_db = self.create_rollup_db(rollup_block_number);
        let mut rollup_block_env = BlockEnv::default();
        sim_env.prev_header.fill_block_env(&mut rollup_block_env);

        let rollup_env = RollupEnv::<RollupAlloyDatabaseProvider, NoOpInspector>::new(
            rollup_db,
            constants,
            &self.config.ru_cfg_env(),
            &rollup_block_env,
        );

        (rollup_env, host_env)
    }

    /// Spawns the simulator task, which ticks along the simulation loop
    /// as it receives block environments.
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
        submit_sender: mpsc::UnboundedSender<SimResult>,
    ) -> JoinHandle<()> {
        debug!("starting simulator task");

        tokio::spawn(async move { self.run_simulator(constants, cache, submit_sender).await })
    }

    /// This function runs indefinitely, waiting for the block environment to be set and checking
    /// if the current slot is valid before building a block and sending it along for to the submit channel.
    ///
    /// If it is authorized for the current slot, then the simulator task
    /// - clones the simulation cache,
    /// - calculates a deadline for block building,
    /// - attempts to build a block using the latest cache and constants,
    /// - then submits the built block through the provided channel.
    ///
    /// If an error occurs during block building or submission, it logs the error and continues the loop.
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
        submit_sender: mpsc::UnboundedSender<SimResult>,
    ) {
        loop {
            // Wait for the block environment to be set
            if self.sim_env.changed().await.is_err() {
                tracing::error!("block_env channel closed - shutting down simulator task");
                return;
            }
            let Some(sim_env) = self.sim_env.borrow_and_update().clone() else { return };

            let span = sim_env.span();
            span_info!(span, "new block environment received");

            // Calculate the deadline for this block simulation.
            // NB: This must happen _after_ taking a reference to the sim cache,
            // waiting for a new block, and checking current slot authorization.
            let finish_by = self.calculate_deadline();
            let sim_cache = cache.clone();

            let Ok(block) = self
                .handle_build(constants.clone(), sim_cache, finish_by, sim_env.clone())
                .instrument(span.clone())
                .await
                .inspect_err(|err| span_error!(span, %err, "error during block build"))
            else {
                continue;
            };

            span_debug!(span, tx_count = block.transactions().len(), "built simulated block");
            let _ = submit_sender.send(SimResult { block, sim_env });
        }
    }

    /// Calculates the deadline for the current block simulation.
    ///
    /// # Returns
    ///
    /// An `Instant` representing the simulation deadline, as calculated by
    /// determining the time left in the current slot and adding that to the
    /// current timestamp in UNIX seconds.
    pub fn calculate_deadline(&self) -> Instant {
        // Get the current timepoint within the slot.
        let timepoint =
            self.slot_calculator().current_point_within_slot().expect("host chain has started");

        // We have the timepoint in seconds into the slot. To find out what's
        // remaining, we need to subtract it from the slot duration
        // we also subtract 3 seconds to account for the sequencer stopping signing.
        let remaining = (self.slot_calculator().slot_duration() - timepoint).saturating_sub(3);

        let deadline = Instant::now() + Duration::from_secs(remaining);
        deadline.max(Instant::now())
    }

    /// Creates an `AlloyDB` instnace from the host provider.
    async fn create_host_db(&self, latest_block_number: u64) -> HostAlloyDatabaseProvider {
        let alloy_db = AlloyDB::new(self.host_provider.clone(), BlockId::from(latest_block_number));

        // Wrap the AlloyDB instance in a WrapDatabaseAsync and return it.
        // This is safe to unwrap because the main function sets the proper runtime settings.
        //
        // See: https://docs.rs/tokio/latest/tokio/attr.main.html
        WrapDatabaseAsync::new(alloy_db).unwrap()
    }

    /// Creates an `AlloyDB` instance from the rollup provider.
    fn create_rollup_db(&self, latest_block_number: u64) -> RollupAlloyDatabaseProvider {
        // Make an AlloyDB instance from the rollup provider with that latest block number
        let alloy_db: AlloyDB<Ethereum, RuProvider> =
            AlloyDB::new(self.ru_provider.clone(), BlockId::from(latest_block_number));

        // Wrap the AlloyDB instance in a WrapDatabaseAsync and return it.
        // This is safe to unwrap because the main function sets the proper runtime settings.
        //
        // See: https://docs.rs/tokio/latest/tokio/attr.main.html
        WrapDatabaseAsync::new(alloy_db).unwrap()
    }
}
