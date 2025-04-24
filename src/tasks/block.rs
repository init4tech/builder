use crate::{
    config::{BuilderConfig, WalletlessProvider},
    tasks::bundler::Bundle,
};
use alloy::{
    consensus::TxEnvelope,
    eips::{BlockId, BlockNumberOrTag::Latest},
    providers::Provider,
};
use signet_sim::{BlockBuild, BuiltBlock, SimCache, SimItem};
use signet_types::{SlotCalculator, config::SignetSystemConstants};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    select,
    sync::{
        RwLock,
        mpsc::{self},
    },
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
    pub ru_provider: WalletlessProvider,
    /// The slot calculator for determining when to wake up and build blocks.
    pub slot_calculator: SlotCalculator,
}

impl Simulator {
    /// Creates a new `Simulator` instance.
    ///
    /// # Arguments
    /// - `config`: The configuration for the builder.
    /// - `ru_provider`: A provider for interacting with the rollup.
    /// - `slot_calculator`: A slot calculator for managing block timing.
    ///
    /// # Returns
    /// A new `Simulator` instance.
    pub fn new(
        config: &BuilderConfig,
        ru_provider: WalletlessProvider,
        slot_calculator: SlotCalculator,
    ) -> Self {
        Self { config: config.clone(), ru_provider, slot_calculator }
    }

    /// Handles building a single block.
    ///
    /// # Arguments
    /// - `constants`: The system constants for the rollup.
    /// - `sim_items`: The simulation cache containing transactions and bundles.
    /// - `finish_by`: The deadline by which the block must be built.
    ///
    /// # Returns
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
    /// - `tx_receiver`: A channel receiver for incoming transactions.
    /// - `bundle_receiver`: A channel receiver for incoming bundles.
    /// - `cache`: The simulation cache to store the received items.
    ///
    /// # Returns
    /// A `JoinHandle` for the basefee updater and a `JoinHandle` for the
    /// cache handler.
    pub fn spawn_cache_task(
        self: Arc<Self>,
        mut tx_receiver: mpsc::UnboundedReceiver<TxEnvelope>,
        mut bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
        cache: SimCache,
    ) -> (JoinHandle<()>, JoinHandle<()>) {
        tracing::debug!("starting up cache handler");

        let shared_price = Arc::new(RwLock::new(0_u64));
        let price_updater = Arc::clone(&shared_price);
        let price_reader = Arc::clone(&shared_price);

        // Update the basefee on a per-block cadence
        let basefee_jh = tokio::spawn(async move {
            loop {
                // calculate next slot position plus a small buffer to
                let time_remaining = self.slot_calculator.slot_duration()
                    - self.slot_calculator.current_timepoint_within_slot()
                    + 1;
                tracing::debug!(time_remaining = ?time_remaining, "sleeping until next slot");

                // wait until that point in time
                sleep(Duration::from_secs(time_remaining)).await;

                // update the basefee with that price
                tracing::debug!("checking latest basefee");
                let resp = self.ru_provider.get_block_by_number(Latest).await;

                if let Ok(maybe_block) = resp {
                    match maybe_block {
                        Some(block) => {
                            let basefee = block.header.base_fee_per_gas.unwrap_or_default();
                            let mut w = price_updater.write().await;
                            *w = basefee;
                            tracing::debug!(basefee = %basefee, "basefee updated");
                        }
                        None => {
                            tracing::debug!("no block found; basefee not updated.")
                        }
                    }
                }
            }
        });

        // Update the sim cache whenever a transaction or bundle is received with respect to the basefee
        let cache_jh = tokio::spawn(async move {
            loop {
                let p = price_reader.read().await;
                select! {
                    maybe_tx = tx_receiver.recv() => {
                        if let Some(tx) = maybe_tx {
                            tracing::debug!(tx = ?tx.hash(), "received transaction");
                            cache.add_item(SimItem::Tx(tx), *p);
                        }
                    }
                    maybe_bundle = bundle_receiver.recv() => {
                        if let Some(bundle) = maybe_bundle {
                            tracing::debug!(bundle = ?bundle.id, "received bundle");
                            cache.add_item(SimItem::Bundle(bundle.bundle), *p);
                        }
                    }
                }
            }
        });

        (basefee_jh, cache_jh)
    }

    /// Spawns the simulator task, which handles the setup and sets the deadline  
    /// for the each round of simulation.
    ///
    /// # Arguments
    /// - `constants`: The system constants for the rollup.
    /// - `cache`: The simulation cache containing transactions and bundles.
    /// - `submit_sender`: A channel sender for submitting built blocks.
    ///
    /// # Returns
    /// A `JoinHandle` for the spawned task.
    pub fn spawn_simulator_task(
        self: Arc<Self>,
        constants: SignetSystemConstants,
        cache: SimCache,
        submit_sender: mpsc::UnboundedSender<BuiltBlock>,
    ) -> JoinHandle<()> {
        tracing::debug!("starting builder task");
        tokio::spawn(async move {
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
        })
    }

    /// Calculates the deadline for the current block simulation.
    ///
    /// # Returns
    /// An `Instant` representing the deadline.
    pub fn calculate_deadline(&self) -> Instant {
        let now = SystemTime::now();
        let unix_seconds = now.duration_since(UNIX_EPOCH).expect("Time went backwards").as_secs();

        Instant::now().checked_add(Duration::from_secs(unix_seconds)).unwrap()
    }

    /// Creates an `AlloyDB` instance from the rollup provider.
    ///
    /// # Returns
    /// An `Option` containing the wrapped database or `None` if an error occurs.
    async fn create_db(&self) -> Option<WrapAlloyDatabaseAsync> {
        let latest = match self.ru_provider.get_block_number().await {
            Ok(block_number) => block_number,
            Err(e) => {
                tracing::error!(error = %e, "failed to get latest block number");
                return None;
            }
        };
        let alloy_db = AlloyDB::new(self.ru_provider.clone(), BlockId::from(latest));
        let wrapped_db = WrapDatabaseAsync::new(alloy_db).unwrap_or_else(|| {
            panic!("failed to acquire async alloy_db; check which runtime you're using")
        });
        Some(wrapped_db)
    }
}

/// The wrapped alloy database type that is compatible with `Db` and `DatabaseRef`.
type WrapAlloyDatabaseAsync = WrapDatabaseAsync<
    AlloyDB<
        alloy::network::Ethereum,
        alloy::providers::fillers::FillProvider<
            alloy::providers::fillers::JoinFill<
                alloy::providers::Identity,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::GasFiller,
                    alloy::providers::fillers::JoinFill<
                        alloy::providers::fillers::BlobGasFiller,
                        alloy::providers::fillers::JoinFill<
                            alloy::providers::fillers::NonceFiller,
                            alloy::providers::fillers::ChainIdFiller,
                        >,
                    >,
                >,
            >,
            alloy::providers::RootProvider,
        >,
    >,
>;

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
