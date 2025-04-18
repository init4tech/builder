use crate::{
    config::{BuilderConfig, WalletlessProvider},
    tasks::{
        bundler::{Bundle, BundlePoller},
        oauth::Authenticator,
        tx_poller::TxPoller,
    },
};
use alloy::{consensus::TxEnvelope, eips::BlockId, providers::Provider};
use signet_sim::{BlockBuild, BuiltBlock, SimCache, SimItem};
use signet_types::config::SignetSystemConstants;
use tokio::{
    select,
    sync::mpsc::{self},
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

/// Ethereum's slot time in seconds.
pub const ETHEREUM_SLOT_TIME: u64 = 12;

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
    /// A poller for fetching transactions.
    pub tx_poller: TxPoller,
    /// A poller for fetching bundles.
    pub bundle_poller: BundlePoller,
}

impl BlockBuilder {
    /// Create a new block builder with the given config.
    pub fn new(
        config: &BuilderConfig,
        authenticator: Authenticator,
        ru_provider: WalletlessProvider,
    ) -> Self {
        Self {
            config: config.clone(),
            ru_provider,
            tx_poller: TxPoller::new(config),
            bundle_poller: BundlePoller::new(config, authenticator),
        }
    }

    /// Spawn the block builder task, returning the inbound channel to it, and
    /// a handle to the running task.
    pub async fn handle_build(
        &self,
        constants: SignetSystemConstants,
        ru_provider: WalletlessProvider,
        sim_items: SimCache,
    ) -> Result<BuiltBlock, mpsc::error::SendError<BuiltBlock>> {
        let db = create_db(ru_provider).await.unwrap();

        // TODO: add real slot calculator
        let finish_by = std::time::Instant::now() + std::time::Duration::from_millis(200);

        dbg!(sim_items.read_best(2));
        dbg!(sim_items.len());

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

    /// Spawns a task that receives transactions and bundles from the pollers and
    /// adds them to the shared cache.
    pub async fn spawn_cache_task(
        &self,
        mut tx_receiver: mpsc::UnboundedReceiver<TxEnvelope>,
        mut bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
        cache: SimCache,
    ) {
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
    }
}

/// Creates an AlloyDB from a rollup provider
async fn create_db(ru_provider: WalletlessProvider) -> Option<WrapAlloyDatabaseAsync> {
    let latest = match ru_provider.get_block_number().await {
        Ok(block_number) => block_number,
        Err(e) => {
            tracing::error!(error = %e, "failed to get latest block number");
            println!("failed to get latest block number");
            // Should this do anything else?
            return None;
        }
    };
    let alloy_db = AlloyDB::new(ru_provider.clone(), BlockId::from(latest));
    let wrapped_db = WrapDatabaseAsync::new(alloy_db).unwrap_or_else(|| {
        panic!("failed to acquire async alloy_db; check which runtime you're using")
    });
    Some(wrapped_db)
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
