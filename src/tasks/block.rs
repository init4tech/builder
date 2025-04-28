use crate::{
    config::{BuilderConfig, RuProvider},
    tasks::bundler::Bundle,
};
use alloy::{
    consensus::TxEnvelope,
    eips::{BlockId, BlockNumberOrTag::Latest},
    network::Ethereum,
    providers::Provider,
};
use signet_sim::{BlockBuild, BuiltBlock, SimCache};
use signet_types::{SlotCalculator, config::SignetSystemConstants};
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
use trevm::{
    NoopBlock,
    revm::{
        context::CfgEnv,
        database::{AlloyDB, WrapDatabaseAsync},
        inspector::NoOpInspector,
        primitives::hardfork::SpecId,
    },
};

/// Pecorino Chain ID used for the Pecorino network.
pub const PECORINO_CHAIN_ID: u64 = 14174;

/// `Simulator` is responsible for periodically building blocks and submitting them for
/// signing and inclusion in the blockchain.
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
    ) -> Result<BuiltBlock, mpsc::error::SendError<BuiltBlock>> {
        tracing::debug!(finish_by = ?finish_by, "starting block simulation");

        let db = self.create_db().await.unwrap();
        let block_build: BlockBuild<_, NoOpInspector> = BlockBuild::new(
            db,
            constants,
            PecorinoCfg {},
            NoopBlock,
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
    pub fn spawn_cache_task(
        self: Arc<Self>,
        tx_receiver: mpsc::UnboundedReceiver<TxEnvelope>,
        bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
        cache: SimCache,
    ) -> (JoinHandle<()>, JoinHandle<()>) {
        tracing::debug!("starting up cache handler");

        let basefee_price = Arc::new(AtomicU64::new(0_u64));
        let basefee_reader = Arc::clone(&basefee_price);

        // Update the basefee on a per-block cadence
        let basefee_jh =
            tokio::spawn(async move { self.basefee_updater(Arc::clone(&basefee_price)).await });

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
        tracing::debug!("checking latest basefee");
        let resp = self.ru_provider.get_block_by_number(Latest).await;
        if let Err(e) = resp {
            tracing::debug!(err = %e, "basefee check failed with rpc error");
            return;
        }

        if let Ok(maybe_block) = resp {
            match maybe_block {
                Some(block) => {
                    let basefee = block.header.base_fee_per_gas.unwrap_or_default();
                    price.store(basefee, Ordering::Relaxed);
                    tracing::debug!(basefee = %basefee, "basefee updated");
                }
                None => {
                    tracing::debug!("no block found; basefee not updated.")
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

            match self.handle_build(constants, sim_cache, finish_by).await {
                Ok(block) => {
                    tracing::debug!(block = ?block, "built block");
                    let _ = submit_sender.send(block);
                }
                Err(e) => {
                    tracing::error!(err = %e, "failed to send block");
                    continue;
                }
            }
        }
    }

    /// Calculates the deadline for the current block simulation.
    ///
    /// # Returns
    ///
    /// An `Instant` representing the deadline.
    pub fn calculate_deadline(&self) -> Instant {
        let now = SystemTime::now();
        let unix_seconds = now.duration_since(UNIX_EPOCH).expect("Time went backwards").as_secs();

        Instant::now().checked_add(Duration::from_secs(unix_seconds)).unwrap()
    }

    /// Creates an `AlloyDB` instance from the rollup provider.
    ///
    /// # Returns
    ///
    /// An `Option` containing the wrapped database or `None` if an error occurs.
    async fn create_db(&self) -> Option<AlloyDatabaseProvider> {
        let latest = match self.ru_provider.get_block_number().await {
            Ok(block_number) => block_number,
            Err(e) => {
                tracing::error!(error = %e, "failed to get latest block number");
                return None;
            }
        };
        let alloy_db: AlloyDB<Ethereum, RuProvider> =
            AlloyDB::new(self.ru_provider.clone(), BlockId::from(latest));
        let wrapped_db: WrapDatabaseAsync<AlloyDB<Ethereum, RuProvider>> =
            WrapDatabaseAsync::new(alloy_db).unwrap_or_else(|| {
                panic!("failed to acquire async alloy_db; check which runtime you're using")
            });
        Some(wrapped_db)
    }
}

/// Continuously updates the simulation cache with incoming transactions and bundles.
///
/// This function listens for new transactions and bundles on their respective
/// channels and adds them to the simulation cache using the latest observed basefee.
///
/// # Arguments
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

/// Configuration struct for Pecorino network values.
#[derive(Debug, Clone)]
pub struct PecorinoCfg {}

impl Copy for PecorinoCfg {}

impl trevm::Cfg for PecorinoCfg {
    /// Fills the configuration environment with Pecorino-specific values.
    ///
    /// # Arguments
    /// - `cfg_env`: The configuration environment to be filled.
    fn fill_cfg_env(&self, cfg_env: &mut CfgEnv) {
        let CfgEnv { chain_id, spec, .. } = cfg_env;

        *chain_id = PECORINO_CHAIN_ID;
        *spec = SpecId::default();
    }
}
