//! `block.rs` contains the Simulator and everything that wires it into an
//! actor that handles the simulation of a stream of bundles and transactions
//! and turns them into valid Pecorino blocks for network submission.
use crate::{
    config::{BuilderConfig, HostProvider, RuProvider},
    tasks::env::SimEnv,
};
use alloy::consensus::Header;
use init4_bin_base::{
    deps::metrics::{counter, histogram},
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
use tracing::{Instrument, Span, debug, instrument};

/// SimResult bundles a BuiltBlock to the BlockEnv it was simulated against.
#[derive(Debug, Clone)]
pub struct SimResult {
    /// The block built with the successfully simulated transactions
    pub block: BuiltBlock,
    /// The block environment the transactions were simulated against.
    pub sim_env: SimEnv,
}

impl SimResult {
    /// Get a reference to the previous host header.
    pub const fn prev_host(&self) -> &Header {
        self.sim_env.prev_host()
    }

    /// Get a reference to the previous rollup header.
    pub const fn prev_rollup(&self) -> &Header {
        self.sim_env.prev_rollup()
    }

    /// Returns the block number of the built block.
    pub const fn rollup_block_number(&self) -> u64 {
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

/// A task that builds blocks based on incoming [`SimEnv`]s and a simulation
/// cache.
#[derive(Debug)]
pub struct SimulatorTask {
    /// Configuration for the builder.
    config: &'static BuilderConfig,
    /// Host Provider to interact with the host chain.
    host_provider: HostProvider,
    /// A provider that cannot sign transactions, used for interacting with the rollup.
    ru_provider: RuProvider,
    /// The block configuration environments on which to simulate
    envs: watch::Receiver<Option<SimEnv>>,
}

impl SimulatorTask {
    /// Create a new `SimulatorTask` instance. This task must be spawned to
    /// begin processing incoming block environments.
    pub async fn new(envs: watch::Receiver<Option<SimEnv>>) -> eyre::Result<Self> {
        let config = crate::config();

        let (host_provider, ru_provider) =
            tokio::try_join!(config.connect_host_provider(), config.connect_ru_provider())?;

        Ok(Self { config, host_provider, ru_provider, envs })
    }

    /// Get the slot calculator.
    pub const fn slot_calculator(&self) -> &SlotCalculator {
        &self.config.slot_calculator
    }

    /// Get the system constants.
    pub const fn constants(&self) -> &SignetSystemConstants {
        &self.config.constants
    }

    /// Build a single block
    ///
    /// Build a block in the sim environment with items from the simulation
    /// cache against the database state. When the `finish_by` deadline is
    /// reached, it stops simulating and returns the block.
    ///
    /// # Arguments
    ///
    /// - `sim_items`: The simulation cache containing transactions and bundles.
    /// - `finish_by`: The deadline by which the block must be built.
    /// - `sim_env`: The block environment to simulate against.
    ///
    /// # Returns
    ///
    /// A `Result` containing the built block or an error.
    #[instrument(skip_all, fields(
        tx_count = sim_items.len(),
        millis_to_deadline = finish_by.duration_since(Instant::now()).as_millis(),
        host_block_number = sim_env.rollup_block_number(),
        rollup_block_number = sim_env.host_block_number(),
    ))]
    pub async fn handle_build(
        &self,
        sim_items: SimCache,
        finish_by: Instant,
        sim_env: &SimEnv,
    ) -> eyre::Result<BuiltBlock> {
        let concurrency_limit = self.config.concurrency_limit();

        let rollup_env = sim_env.sim_rollup_env(self.constants(), self.ru_provider.clone());
        let host_env = sim_env.sim_host_env(self.constants(), self.host_provider.clone());

        let ru_number = rollup_env.block().number;
        let host_number = host_env.block().number;
        debug!(?ru_number, ?host_number, "starting block simulation");

        let block_build = BlockBuild::new(
            rollup_env,
            host_env,
            finish_by,
            concurrency_limit,
            sim_items,
            self.config.rollup_block_gas_limit,
            self.config.max_host_gas(sim_env.prev_host().gas_limit),
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
        cache: SimCache,
        submit_sender: mpsc::UnboundedSender<SimResult>,
    ) -> JoinHandle<()> {
        debug!("starting simulator task");

        tokio::spawn(async move { self.run_simulator(cache, submit_sender).await })
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
        cache: SimCache,
        submit_sender: mpsc::UnboundedSender<SimResult>,
    ) {
        loop {
            // Wait for the block environment to be set
            if self.envs.changed().await.is_err() {
                tracing::error!("block_env channel closed - shutting down simulator task");
                return;
            }
            let Some(sim_env) = self.envs.borrow_and_update().clone() else { return };

            let span = sim_env.span();
            span_info!(span, "new block environment received");

            // Calculate the deadline for this block simulation.
            // NB: This must happen _after_ taking a reference to the sim cache,
            // waiting for a new block, and checking current slot authorization.
            let finish_by = self.calculate_deadline();
            let sim_cache = cache.clone();

            let Ok(block) = self
                .handle_build(sim_cache, finish_by, &sim_env)
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

    /// Calculates the deadline for the current block simulation in milliseconds.
    ///
    /// # Returns
    ///
    /// An `Instant` representing the simulation deadline as calculated by determining
    /// the milliseconds  left in the current slot and adding that to the current
    /// timestamp in UNIX seconds.
    pub fn calculate_deadline(&self) -> Instant {
        // Get the current number of milliseconds into the slot.
        let timepoint_ms =
            self.slot_calculator().current_point_within_slot_ms().expect("host chain has started");
        let slot_duration = Duration::from_secs(self.slot_calculator().slot_duration()).as_millis();
        let elapsed_in_slot = Duration::from_millis(timepoint_ms);
        let query_cutoff_buffer = Duration::from_millis(self.config.block_query_cutoff_buffer);

        // To find the remaining slot time, subtract the timepoint from the slot duration.
        // Then subtract the block query cutoff buffer from the slot duration to account for
        // the sequencer stopping signing.
        let remaining = slot_duration
            .saturating_sub(elapsed_in_slot.as_millis())
            .saturating_sub(query_cutoff_buffer.as_millis());

        // The deadline is then calculated by adding the remaining time from this instant.
        // NB: Downcast is okay because u64 will work for 500 million+ years.
        let deadline = Instant::now() + Duration::from_millis(remaining as u64);
        deadline.max(Instant::now())
    }
}
