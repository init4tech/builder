use crate::{
    config::{BuilderConfig, WalletlessProvider},
    tasks::bundler::Bundle,
};
use alloy::{consensus::TxEnvelope, eips::BlockId, providers::Provider};
use signet_sim::{BlockBuild, BuiltBlock, SimCache, SimItem};
use signet_types::{SlotCalculator, config::SignetSystemConstants};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    select,
    sync::mpsc::{self},
    task::JoinHandle,
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

/// Pecorino Chain ID
pub const PECORINO_CHAIN_ID: u64 = 14174;

/// BlockBuilder is a task that periodically builds a block then sends it for
/// signing and submission.
#[derive(Debug)]
pub struct BlockBuilder {
    /// Configuration.
    pub config: BuilderConfig,
    /// A provider that cannot sign transactions.
    pub ru_provider: WalletlessProvider,
    /// The slot calculator for waking up and sleeping the builder correctly
    pub slot_calculator: SlotCalculator,
}

impl BlockBuilder {
    /// Creates a new block builder that builds blocks based on the given provider.
    pub fn new(
        config: &BuilderConfig,
        ru_provider: WalletlessProvider,
        slot_calculator: SlotCalculator,
    ) -> Self {
        Self { config: config.clone(), ru_provider, slot_calculator }
    }

    /// Handles building a single block.
    pub async fn handle_build(
        &self,
        constants: SignetSystemConstants,
        sim_items: SimCache,
        finish_by: Instant,
    ) -> Result<BuiltBlock, mpsc::error::SendError<BuiltBlock>> {
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

        Ok(block)
    }

    /// Scans the tx and bundle receivers for new items and adds them to the cache.
    pub fn spawn_cache_handler(
        self: Arc<Self>,
        mut tx_receiver: mpsc::UnboundedReceiver<TxEnvelope>,
        mut bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
        cache: SimCache,
    ) -> JoinHandle<()> {
        let jh = tokio::spawn(async move {
            loop {
                select! {
                    maybe_tx = tx_receiver.recv() => {
                        if let Some(tx) = maybe_tx {
                            cache.add_item(SimItem::Tx(tx));
                        }
                    }
                    maybe_bundle = bundle_receiver.recv() => {
                        if let Some(bundle) = maybe_bundle {
                            cache.add_item(SimItem::Bundle(bundle.bundle));
                        }
                    }
                }
            }
        });
        jh
    }

    /// Spawns the block building task.
    pub fn spawn_builder_task(
        self: Arc<Self>,
        constants: SignetSystemConstants,
        cache: SimCache,
        submit_sender: mpsc::UnboundedSender<BuiltBlock>,
    ) -> JoinHandle<()> {
        let jh = tokio::spawn(async move {
            loop {
                let sim_cache = cache.clone();

                let finish_by = self.calculate_deadline();
                tracing::info!("simulating until target slot deadline");

                // sleep until next wake period
                tracing::info!("starting block build");
                match self.handle_build(constants, sim_cache, finish_by).await {
                    Ok(block) => {
                        let _ = submit_sender.send(block);
                    }
                    Err(e) => {
                        tracing::error!(err = %e, "failed to send block");
                        continue;
                    }
                }
            }
        });
        jh
    }

    /// Returns the instant at which simulation must stop.
    pub fn calculate_deadline(&self) -> Instant {
        let now = SystemTime::now();
        let unix_seconds = now.duration_since(UNIX_EPOCH).expect("Time went backwards").as_secs();

        Instant::now().checked_add(Duration::from_secs(unix_seconds)).unwrap()
    }

    /// Creates an AlloyDB from a rollup provider
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

/// The wrapped alloy database type that is compatible with Db + DatabaseRef
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

/// Configuration struct for Pecorino network values
#[derive(Debug, Clone)]
pub struct PecorinoCfg {}

impl Copy for PecorinoCfg {}

impl trevm::Cfg for PecorinoCfg {
    fn fill_cfg_env(&self, cfg_env: &mut CfgEnv) {
        let CfgEnv { chain_id, spec, .. } = cfg_env;

        *chain_id = PECORINO_CHAIN_ID;
        *spec = SpecId::default();
    }
}
