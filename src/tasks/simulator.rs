use super::{block::InProgressBlock, bundler::Bundle};
use crate::config::{BuilderConfig, WalletlessProvider};
use alloy::{
    consensus::TxEnvelope, eips::BlockId, network::Ethereum, providers::Provider,
    transports::BoxTransport,
};
use eyre::Result;
use reqwest::Url;
use revm::{
    db::{AlloyDB, CacheDB},
    primitives::{ResultAndState, U256},
    DatabaseCommit, EvmBuilder,
};
use std::{
    marker::PhantomData,
    sync::{Arc, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    select,
    sync::mpsc::{self, UnboundedReceiver},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use trevm::{
    revm::{primitives::EVMError, Database},
    Block as TrevmBlock, BlockDriver, Cfg, DbConnect, EvmFactory, EvmNeedsBlock, NoopBlock,
    NoopCfg, TrevmBuilder, Tx,
};

/// Ethereum's slot time in seconds
pub const ETHEREUM_SLOT_TIME: u64 = 12;

#[derive(Debug, Clone)]
pub struct Simulator<Ef, C, B> {
    pub ru_provider: WalletlessProvider,
    pub config: BuilderConfig,
    _marker: PhantomData<(Ef, C, B)>,
}

#[derive(Debug, Clone)]
pub struct EvmPool<Ef, C, B> {
    evm: EvmCtx<Ef, C, B>,
}

impl<Ef, C, B> EvmPool<Ef, C, B>
where
    Ef: for<'a> EvmFactory<'a> + Send + 'static,
    C: Cfg + 'static,
    B: TrevmBlock + 'static,
{
    pub fn weak_evm(&self) -> Weak<EvmCtxInner<Ef, C, B>> {
        Arc::downgrade(&self.evm.0)
    }
}

/// Defines the SimulatorDatabase type for ease of use and clarity
pub type SimulatorDatabase = CacheDB<AlloyDB<BoxTransport, Ethereum, WalletlessProvider>>;

/// Error type for the Simulator
#[derive(Error, Debug)]
pub enum SimulatorError {
    #[error("Database error: {0}")]
    DatabaseError(#[from] EVMError<<SimulatorDatabase as Database>::Error>),
    #[error("Other error: {0}")]
    Other(#[from] eyre::Report),
}

/// Sims and evals a bundle against a given database state,
/// then returns that bundle, its score, and the updated EVM state.
struct SimAndEvalResult<'a, Ext, Db: Database + DatabaseCommit> {
    pub bundle: &'a Bundle,
    pub score: U256,
    pub resultant_state: &'a trevm::EvmNeedsBlock<'a, Ext, Db>,
}

/// A shared EVM state
#[derive(Debug, Clone, Default)]
pub struct EvmCtxInner<Ef, C, B> {
    evm_factory: Ef,
    config: C,
    block: B,
}

/// Creates an ARC over the EVM context
#[derive(Debug, Clone)]
pub struct EvmCtx<Ef, C, B>(Arc<EvmCtxInner<Ef, C, B>>);

/// Evaluation result that is orderable over the Score type
pub struct EvalResult<T, Score: PartialOrd + Ord = U256> {
    pub tx: Arc<T>,
    pub result: ResultAndState,
    pub score: Score,
}

pub struct Best<T, Score: PartialOrd + Ord = U256> {
    pub item: T,
    pub result: ResultAndState,
    pub score: Score,
}

/// Defines the eval function
async fn evaluate<Ef, C, B, T, F>(
    evm: Weak<EvmCtxInner<Ef, C, B>>,
    tx: Weak<T>,
    evaluator: F,
) -> Option<EvalResult<T>>
where
    Ef: for<'a> EvmFactory<'a> + Send + 'static,
    C: Cfg + 'static,
    B: TrevmBlock + 'static,
    T: Tx + 'static,
    F: for<'a> Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
{
    let evm = evm.upgrade()?;
    let tx = tx.upgrade()?;

    let result_and_state = evm.evm_factory.run(&evm.config, &evm.block, tx.as_ref()).ok()?;
    let score = evaluator(&result_and_state);

    Some(EvalResult { tx, result: result_and_state, score })
}

async fn create_pool<Ef, C, B>(
    ru_provider: WalletlessProvider,
    config: BuilderConfig,
) -> Result<EvmPool<Simulator<Ef, NoopCfg, NoopBlock>, NoopCfg, NoopBlock>>
where
    Ef: Send + Sync + for<'b> EvmFactory<'b>,
{
    let sim = Simulator::<Ef, NoopCfg, NoopBlock>::new(ru_provider, config).await?;
    let pool = EvmPool {
        evm: EvmCtx(Arc::new(EvmCtxInner { evm_factory: sim, config: NoopCfg, block: NoopBlock })),
    };
    Ok(pool)
}

impl<'a, Ef, C, B> EvmFactory<'a> for Simulator<Ef, C, B>
where
    Simulator<Ef, C, B>: DbConnect<'a>,
    C: Cfg + 'static,
    B: TrevmBlock + 'static,
{
    type Ext = ();

    // TODO: Implement this function for the Simulator with a TrevmBuilder I think?
    fn create(
        &'a self,
    ) -> std::result::Result<trevm::EvmNeedsCfg<'a, Self::Ext, Self::Database>, Self::Error> {
        todo!()
    }
}

impl<'a, Ef, C, B> DbConnect<'a> for Simulator<Ef, C, B>
where
    Ef: for<'b> EvmFactory<'b> + Send + Sync + 'static,
    C: Cfg + 'static,
    B: TrevmBlock + 'static,
{
    type Database = SimulatorDatabase;
    type Error = SimulatorError;

    fn connect(&'a self) -> std::result::Result<Self::Database, Self::Error> {
        let db = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(self.get_latest_db())
            .map_err(SimulatorError::Other)?;
        Ok(db)
    }
}

impl<'a, Ef, C, B> Simulator<Ef, C, B>
where
    Ef: for<'b> EvmFactory<'b> + Send + Sync,
    C: Cfg + 'static,
    B: TrevmBlock + 'static,
{
    /// Creates a new simulator at the latest block number.
    pub async fn new(ru_provider: WalletlessProvider, config: BuilderConfig) -> Result<Self> {
        Ok(Self { ru_provider, config, _marker: PhantomData })
    }

    /// Returns a prepared Simulator database out of the ru_provider at the latest block number
    pub async fn get_latest_db(&self) -> eyre::Result<SimulatorDatabase> {
        let latest = self.ru_provider.clone().get_block_number().await?;
        if let Some(db) = AlloyDB::new(self.ru_provider.clone(), BlockId::from(latest)) {
            Ok(CacheDB::new(db))
        } else {
            Err(eyre::eyre!("failed to create alloyDB from ru_provider"))
        }
    }

    pub async fn spawn_simulation(
        &self,
        mut inbound: UnboundedReceiver<Arc<TxEnvelope>>,
        deadline: Instant,
    ) -> JoinHandle<()> {
        let jh = tokio::spawn(async move {
            let cancel = tokio::time::sleep_until(deadline);
            tokio::pin!(cancel);

            let mut best: Option<Best<InProgressBlock>> = None;

            let result = create_pool::<Ef, C, B>(self.ru_provider, self.config).await;
            let pool = match result {
                Ok(evm_pool) => evm_pool,
                Err(e) => {
                    tracing::error!("failed to create evm pool");
                    return;
                }
            };

            loop {
                select! {
                    biased;
                    // Break when cancel is received
                    _ = &mut cancel => break,
                    tx = inbound.recv() => {
                        if let Some(tx) = tx {
                            let weak_tx = Arc::downgrade(&tx);
                            let evm = pool.weak_evm();
                            let eval = evaluator.clone();
                        };
                    }
                }
            }
        });
        jh
    }

    /// Spawn a new Simulator that receives bundles and transactions and simulates them into finalized
    /// blocks for later submission to the network.
    pub async fn spawn(
        self,
        mut inbound_bundles: mpsc::UnboundedReceiver<Arc<Bundle>>,
        mut inbound_txs: mpsc::UnboundedReceiver<Arc<TxEnvelope>>,
        submit_channel: mpsc::UnboundedSender<InProgressBlock>,
    ) -> JoinHandle<()> {
        let jh = tokio::spawn(async move {
            let timer = Timer::new(self.config.clone());
            let candidate_block: Option<InProgressBlock> = None;

            loop {
                // Kick off simulation with given deadline
                let next_target_slot = timer.clone().secs_to_next_target();
                let deadline = Duration::from_secs(next_target_slot);
                let sleep = tokio::time::sleep(deadline);
                tokio::pin!(sleep);

                // Wait for the best block to be found within the given deadline
                tracing::info!(deadline = ?deadline, "starting simulation");
                select! {
                    biased;
                    _ = &mut sleep => {
                        // TODO: send best candidate block on submit channel
                        break;
                    },
                    // Listen for incoming transactions
                    tx = inbound_txs.recv() => {
                        if let Some(tx) = tx {
                            tracing::debug!(tx = ?tx, "tx received");
                        }
                    }
                    // Listen for incoming bundles
                    bundle = inbound_bundles.recv() => {
                        if let Some(bundle) = bundle {
                            tracing::debug!(bundle = ?bundle, "bundle received");
                        }
                    }
                }
            }
        });
        jh
    }
}

/// Evaluates the value of a bundle and returns its score for sorting purposes
fn evaluator<Db: Database + DatabaseCommit>(state: &EvmNeedsBlock<'static, (), Db>) -> U256 {
    // Implement the evaluation logic here
    todo!()
}

/// Timer implements the logic for predicting time to next slot
#[derive(Clone)]
pub struct Timer {
    pub config: BuilderConfig,
}

impl Timer {
    // Create a new block builder with the given config.
    pub fn new(config: BuilderConfig) -> Self {
        Self { config }
    }

    // Calculate the duration in seconds until the beginning of the next block slot.
    fn secs_to_next_slot(&self) -> u64 {
        let curr_timestamp: u64 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let current_slot_time = (curr_timestamp - self.config.chain_offset) % ETHEREUM_SLOT_TIME;
        (ETHEREUM_SLOT_TIME - current_slot_time) % ETHEREUM_SLOT_TIME
    }

    // Add a buffer to the beginning of the block slot.
    pub fn secs_to_next_target(&self) -> u64 {
        self.secs_to_next_slot() + self.config.target_slot_time
    }
}

/// Creates an extractor from a generic Db that gives you access to a trevm environment.
pub fn create_extractor<Db>() -> impl BlockExtractor<(), Db>
where
    Db: Database + DatabaseCommit + Send + 'static,
{
    BuilderBlockExtractor {}
}

pub trait BlockExtractor<Ext, Db: Database + DatabaseCommit>: Send + Sync + 'static {
    type Driver: BlockDriver<Ext, Error<Db>: core::error::Error>;

    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, Ext, Db>;

    fn extract(&mut self, bytes: &[u8]) -> Self::Driver;
}

impl<Db> BlockExtractor<(), Db> for BuilderBlockExtractor
where
    Db: Database + DatabaseCommit + Send + 'static,
{
    type Driver = Block;

    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, (), Db> {
        trevm::revm::EvmBuilder::default().with_db(db).build_trevm().fill_cfg(&NoopCfg)
    }

    // Takes a a slice of bytes and decodes them as TxEnvelope types
    fn extract(&mut self, bytes: &[u8]) -> Self::Driver {
        let txs: Vec<TxEnvelope> =
            alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap_or_default();
        Block(txs, NoopBlock)
    }
}

/// Block extractor for the Builder
pub struct BuilderBlockExtractor {}

pub struct Block(Vec<TxEnvelope>, NoopBlock);

impl<Ext> BlockDriver<Ext> for Block {
    type Block = NoopBlock;
    type Error<Db: Database> = Error<Db>;

    fn block(&self) -> &Self::Block {
        &NoopBlock
    }

    fn run_txns<'a, Db: Database + DatabaseCommit>(
        &mut self,
        mut trevm: trevm::EvmNeedsTx<'a, Ext, Db>,
    ) -> trevm::RunTxResult<'a, Ext, Db, Self> {
        for tx in self.0.iter() {
            if tx.recover_signer().is_ok() {
                todo!()
            }
        }
        Ok(trevm)
    }

    fn post_block<Db: Database + DatabaseCommit>(
        &mut self,
        _trevm: &trevm::EvmNeedsBlock<'_, Ext, Db>,
    ) -> Result<(), Self::Error<Db>> {
        Ok(())
    }
}

/// Error implementation
pub struct Error<Db: Database>(EVMError<Db::Error>);

impl<Db> From<EVMError<Db::Error>> for Error<Db>
where
    Db: Database,
{
    fn from(e: EVMError<Db::Error>) -> Self {
        Self(e)
    }
}

impl<Db: Database> core::error::Error for Error<Db> {}

impl<Db: Database> core::fmt::Debug for Error<Db> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Error")
    }
}

impl<Db: Database> core::fmt::Display for Error<Db> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Error")
    }
}
