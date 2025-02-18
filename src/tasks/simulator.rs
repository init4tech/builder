use alloy::consensus::TxEnvelope;
use alloy::primitives::U256;
use revm::db::EmptyDBTyped;
use revm::{DatabaseRef, InMemoryDB};
use revm::db::CacheDB;
use trevm::db::ConcurrentStateInfo;
use std::{
    convert::Infallible,
    sync::{Arc, Weak},
};
use tokio::{sync::mpsc::UnboundedReceiver, task::JoinSet};
use trevm::{
    self, db::ConcurrentState, revm::primitives::ResultAndState, Block, Cfg, EvmFactory, Tx,
};
use trevm::{
    revm::{primitives::EVMError, Database, DatabaseCommit},
    BlockDriver, DbConnect, NoopBlock, NoopCfg, TrevmBuilder,
};

/// Defines the SimulatorDatabase type for ease of use and clarity
// pub type SimulatorDatabase = ConcurrentState<CacheDB<InMemoryDB>>;

/// Evm context inner
#[derive(Debug, Clone)]
pub struct EvmCtxInner<Ef, C, B> {
    evm_factory: Ef,
    cfg: C,
    block: B,
}

/// Evm evaluation context
#[derive(Debug, Clone)]
pub struct EvmCtx<Ef, C, B>(Arc<EvmCtxInner<Ef, C, B>>);

/// Evm simulation pool
#[derive(Debug, Clone)]
pub struct EvmPool<Ef, C, B> {
    evm: EvmCtx<Ef, C, B>,
}

impl<Ef, C, B> EvmPool<Ef, C, B>
where
    Ef: for<'a> EvmFactory<'a> + Send + 'static,
    C: Cfg + 'static,
    B: Block + 'static,
{
    /// Creates a new Evm Pool from the given factory and configs
    pub fn new(evm_factory: Ef, cfg: C, block: B) -> EvmPool<Ef, C, B>
    where
        Ef: for<'a> EvmFactory<'a> + Send + 'static,
        C: Cfg + 'static,
        B: Block + 'static,
    {
        let inner = EvmCtxInner { evm_factory, cfg, block };
        let evm = EvmCtx(Arc::new(inner));
        println!("evm factory - making new evm");
        EvmPool { evm }
    }

    /// Obtains a weak reference to the evm that can be upgrade to run threaded simulation
    fn weak_evm(&self) -> Weak<EvmCtxInner<Ef, C, B>> {
        println!("obtaining weak evm");
        Arc::downgrade(&self.evm.0)
    }
}

fn eval_fn<Ef, C, B, T, F>(
    evm: Weak<EvmCtxInner<Ef, C, B>>,
    tx: Weak<T>,
    evaluator: F,
) -> Option<Best<T>>
where
    Ef: for<'a> EvmFactory<'a> + Send + 'static,
    C: Cfg + 'static,
    B: Block + 'static,
    T: Tx + 'static,
    F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
{
    println!("eval_fn running - evm: {:?}", evm);
    
    // If none, then simulation is over.
    let evm = evm.upgrade()?;
    println!("evm upgraded");

    // If none, tx can be skipped
    let tx = tx.upgrade()?;
    println!("tx upgraded");

    // If none, then tx errored, and can be skipped.
    let result = evm.evm_factory.run(&evm.cfg, &evm.block, tx.as_ref()).ok()?;
    // Best never comes back because run never returns. Why not? 

    // TODO: Result is never returning - but eval_fn is definitely running. 
    // So what is happening? 
    println!("result: {:?}", &result); 

    let score = evaluator(&result);
    println!("score: {}", score);

    Some(Best { tx, result, score })
}

pub struct Best<T, Score: PartialOrd + Ord = U256> {
    pub tx: Arc<T>,
    pub result: ResultAndState,
    pub score: Score,
}

impl<Ef, C, B> EvmPool<Ef, C, B>
where
    Ef: for<'a> EvmFactory<'a> + Send + 'static,
    C: Cfg + 'static,
    B: Block + 'static,
{
    /// Spawn a task that will evaluate the best candidate from a channel of
    /// candidates.
    pub fn spawn<T, F>(
        self,
        mut inbound_tx: UnboundedReceiver<Arc<T>>,
        evaluator: F,
        deadline: tokio::time::Instant,
    ) -> tokio::task::JoinHandle<Option<Best<T>>>
    where
        T: Tx + 'static,
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static + Clone,
    {
        tokio::spawn(async move {
            let mut futs = JoinSet::new();
            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            let mut best: Option<Best<T>> = None;

            loop {
                tokio::select! {
                    biased;
                    _ = &mut sleep => {
                        println!("simulation deadline exceeded");
                        break
                    },
                    tx = inbound_tx.recv() => {
                        let tx = match tx {
                            Some(tx) => tx,
                            None => break,
                        };

                        println!("receiving transaction");

                        let weak_tx = Arc::downgrade(&tx);
                        let evm = self.weak_evm();
                        let eval = evaluator.clone();
                        futs.spawn_blocking(|| eval_fn(evm, weak_tx, eval));
                    }
                    Some(Ok(Some(candidate))) = futs.join_next() => {
                        println!("candidate used gas: {:?}", candidate.result.result.gas_used());
                        if candidate.score > best.as_ref().map(|b| b.score).unwrap_or_default() {
                            best = Some(candidate);
                        }
                    }
                }
            }
            best
        })
    }
}

///
/// Simulator Factory
///

#[derive(Clone)]
pub struct SimulatorFactory<Db, Ext> {
    pub db: Db,
    pub ext: Ext,
}

impl<Db, Ext> SimulatorFactory<Db, Ext> {
    pub fn new(db: Db, ext: Ext) -> Self {
        Self { db, ext }
    }
}

// Wraps a Db into an EvmFactory compatible [`Database`]
impl<'a, Db, Ext> DbConnect<'a> for SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Sync,
    Ext: Sync,
{
    type Database = ConcurrentState<CacheDB<Db>>;
    type Error = Infallible;

    fn connect(&'a self) -> Result<Self::Database, Self::Error> {
        println!("connect - function called");
        let cache: CacheDB<Db>= CacheDB::new(self.db.clone());
        let concurrent_db = ConcurrentState::new(cache, ConcurrentStateInfo::default());
        println!("connect - concurrent db created");
        Ok(concurrent_db)
    }
}

impl<'a, Db, Ext> EvmFactory<'a> for SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Sync + Send + 'static,
    Ext: Sync + Clone,
{
    type Ext = ();

    fn create(&'a self) -> Result<trevm::EvmNeedsCfg<'a, Self::Ext, Self::Database>, Self::Error> {
        println!("create - function called");
        // let db = InMemoryDB::new(EmptyDBTyped::new());
        // let cache = CacheDB::new(db);
        let cache = CacheDB::new(self.db.clone());
        let concurrent_db = ConcurrentState::new(cache, ConcurrentStateInfo::default());
        println!("create - cloned database");
        let t = trevm::revm::EvmBuilder::default().with_db(concurrent_db).build_trevm();
        println!("create - trevm created {:?}", t);
        Ok(t)
    }
}

///
/// Extractor
///

/// A trait for extracting transactions from a block.
pub trait BlockExtractor<Ext, Db: Database + DatabaseCommit>: Send + Sync + 'static {
    type Driver: BlockDriver<Ext, Error<Db>: core::error::Error>;

    /// Instantiate an configure a new [`trevm`] instance.
    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, Ext, Db>;

    /// Extracts transactions from the source.
    ///
    /// Extraction is infallible. Worst case it should return a no-op driver.
    fn extract(&mut self, bytes: &[u8]) -> Self::Driver;
}

/// An implementation of BlockExtractor for Simulation purposes
pub struct SimulatorExtractor {}

impl<Db> BlockExtractor<(), Db> for SimulatorExtractor
where
    Db: Database + DatabaseCommit + Send + 'static,
{
    type Driver = SimBundle;

    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, (), Db> {
        trevm::revm::EvmBuilder::default().with_db(db).build_trevm().fill_cfg(&NoopCfg)
    }

    fn extract(&mut self, bytes: &[u8]) -> Self::Driver {
        #[allow(clippy::useless_asref)]
        let txs: Vec<TxEnvelope> =
            alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap_or_default();
        SimBundle(txs, NoopBlock)
    }
}

pub fn create_simulator_extractor<Db>() -> SimulatorExtractor
where
    Db: Database + DatabaseCommit + Send + 'static,
{
    SimulatorExtractor {}
}

///
/// Bundle Driver
///

pub struct SimBundle(Vec<TxEnvelope>, NoopBlock);

pub struct SimTxEnvelope(pub TxEnvelope);

impl Tx for SimTxEnvelope {
    fn fill_tx_env(&self, tx_env: &mut revm::primitives::TxEnv) {
        println!("fillng tx env {:?}", tx_env);
        let revm::primitives::TxEnv { ..  } = tx_env;
    }
}

impl<Ext> BlockDriver<Ext> for SimBundle {
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
                let sim_tx = SimTxEnvelope(tx.clone());
                let t = match trevm.run_tx(&sim_tx) {
                    Ok(t) => t,
                    Err(e) => {
                        if e.is_transaction_error() {
                            return Ok(e.discard_error());
                        } else {
                            return Err(e.err_into());
                        }
                    }
                };
                (_, trevm) = t.accept();
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

///
/// Impls
///

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
