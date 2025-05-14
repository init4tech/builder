//! `block.rs` contains the Simulator and everything that wires it into an
//! actor that handles the simulation of a stream of bundles and transactions
//! and turns them into valid Pecorino blocks for network submission.
use super::cfg::PecorinoBlockEnv;
use crate::{
    config::{BuilderConfig, RuProvider},
    tasks::{block::cfg::PecorinoCfg, bundler::Bundle},
};
use alloy::{
    consensus::TxEnvelope,
    eips::{BlockId, BlockNumberOrTag::Latest},
    network::Ethereum,
    providers::Provider,
};
use chrono::{DateTime, Utc};
use eyre::{Context, bail};
use init4_bin_base::{
    deps::tracing::{debug, error, info, warn},
    utils::calc::SlotCalculator,
};
use signet_sim::{BlockBuild, BuiltBlock, SimCache};
use signet_types::constants::SignetSystemConstants;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    select,
    sync::mpsc::{self},
    task::JoinHandle,
    time::sleep,
};
use trevm::revm::{
    database::{AlloyDB, WrapDatabaseAsync},
    inspector::NoOpInspector,
};

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
    ) -> eyre::Result<BuiltBlock> {
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
        debug!(block = ?block, "finished block simulation");

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
        &self,
        tx_receiver: mpsc::UnboundedReceiver<TxEnvelope>,
        bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
        cache: SimCache,
    ) -> (JoinHandle<()>, JoinHandle<()>) {
        debug!("starting up cache handler");

        let basefee_price = Arc::new(AtomicU64::new(0_u64));
        let basefee_reader = Arc::clone(&basefee_price);
        let fut = self.basefee_updater_fut(basefee_price);

        // Update the basefee on a per-block cadence
        let basefee_jh = tokio::spawn(fut);

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
    fn basefee_updater_fut(&self, price: Arc<AtomicU64>) -> impl Future<Output = ()> + use<> {
        let slot_calculator = self.slot_calculator;
        let ru_provider = self.ru_provider.clone();

        async move {
            debug!("starting basefee updater");
            loop {
                // calculate start of next slot plus a small buffer
                let time_remaining = slot_calculator.slot_duration()
                    - slot_calculator.current_timepoint_within_slot()
                    + 1;
                debug!(time_remaining = ?time_remaining, "basefee updater sleeping until next slot");

                // wait until that point in time
                sleep(Duration::from_secs(time_remaining)).await;

                // update the basefee with that price
                let resp = ru_provider.get_block_by_number(Latest).await.inspect_err(|e| {
                    error!(error = %e, "RPC error during basefee update");
                });

                if let Ok(Some(block)) = resp {
                    let basefee = block.header.base_fee_per_gas.unwrap_or(0);
                    price.store(basefee, Ordering::Relaxed);
                    debug!(basefee = basefee, "basefee updated");
                } else {
                    warn!("get basefee failed - an error likely occurred");
                }
            }
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
        self,
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
                    error!(err = %err, "failed to configure next block");
                    break;
                }
            };
            info!(block_env = ?block_env, "created block");

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

    /// Prepares the next block environment.
    ///
    /// Prepares the next block environment to load into the simulator by fetching the latest block number,
    /// assigning the correct next block number, checking the basefee, and setting the timestamp,
    /// reward address, and gas configuration for the block environment based on builder configuration.
    ///
    /// # Arguments
    ///
    /// - finish_by: The deadline at which block simulation will end.
    async fn next_block_env(&self, finish_by: Instant) -> eyre::Result<PecorinoBlockEnv> {
        let remaining = finish_by.duration_since(Instant::now());
        let finish_time = SystemTime::now() + remaining;
        let deadline: DateTime<Utc> = finish_time.into();
        debug!(deadline = %deadline, "preparing block env");

        // Fetch the latest block number and increment it by 1
        let latest_block_number = match self.ru_provider.get_block_number().await {
            Ok(num) => num,
            Err(err) => {
                error!(%err, "RPC error during block build");
                bail!(err)
            }
        };
        debug!(next_block_num = latest_block_number + 1, "preparing block env");

        // Fetch the basefee from previous block to calculate gas for this block
        let basefee = match self.get_basefee().await? {
            Some(basefee) => basefee,
            None => {
                warn!("get basefee failed - RPC error likely occurred");
                todo!()
            }
        };
        debug!(basefee = basefee, "setting basefee");

        // Craft the Block environment to pass to the simulator
        let block_env = PecorinoBlockEnv::new(
            self.config.clone(),
            latest_block_number + 1,
            deadline.timestamp() as u64,
            basefee,
        );
        debug!(block_env = ?block_env, "prepared block env");

        Ok(block_env)
    }

    /// Returns the basefee of the latest block.
    ///
    /// # Returns
    ///
    /// The basefee of the previous (latest) block if the request was successful,
    /// or a sane default if the RPC failed.
    async fn get_basefee(&self) -> eyre::Result<Option<u64>> {
        let Some(block) =
            self.ru_provider.get_block_by_number(Latest).await.wrap_err("basefee error")?
        else {
            return Ok(None);
        };

        debug!(basefee = ?block.header.base_fee_per_gas, "basefee found");
        Ok(block.header.base_fee_per_gas)
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
                    debug!(tx = ?tx.hash(), "received transaction");
                    cache.add_item(tx, p);
                }
            }
            maybe_bundle = bundle_receiver.recv() => {
                if let Some(bundle) = maybe_bundle {
                    debug!(bundle = ?bundle.id, "received bundle");
                    cache.add_item(bundle.bundle, p);
                }
            }
        }
    }
}
