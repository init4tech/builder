use alloy::consensus::TxEnvelope;
use alloy::primitives::U256;
use alloy_rlp::Encodable;
use revm::{db::CacheDB, primitives::Bytes, DatabaseRef};
use std::{
    convert::Infallible,
    sync::{Arc, Weak},
};
use tokio::{select, sync::mpsc::UnboundedReceiver, task::JoinSet};
use trevm::db::ConcurrentStateInfo;
use trevm::{
    self, db::ConcurrentState, revm::primitives::ResultAndState, Block, Cfg, EvmFactory, Tx,
};
use trevm::{
    revm::{primitives::EVMError, Database, DatabaseCommit},
    BlockDriver, DbConnect, NoopBlock, NoopCfg, TrevmBuilder,
};

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
        EvmPool { evm }
    }

    /// Obtains a weak reference to the evm that can be upgrade to run threaded simulation
    fn weak_evm(&self) -> Weak<EvmCtxInner<Ef, C, B>> {
        Arc::downgrade(&self.evm.0)
    }
}

// fn eval_fn<Ef, C, B, T, F>(
//     evm: Weak<EvmCtxInner<Ef, C, B>>,
//     tx: Arc<T>,
//     evaluator: F,
// ) -> Option<Best<T>>
// where
//     Ef: for<'a> EvmFactory<'a> + Send + 'static,
//     C: Cfg + 'static,
//     B: Block + 'static,
//     T: Tx + 'static,
//     F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
// {
//     tracing::info!("eval_fn running - evm: {:?}", evm);

//     // If none, then simulation is over.
//     let evm = evm.upgrade()?;
//     tracing::info!("evm upgraded");

//     // If none, then tx errored, and can be skipped.
//     let result = evm.evm_factory.run(&evm.cfg, &evm.block, tx.as_ref()).ok()?;
//     result.state.
//     tracing::info!("result: {:?}", &result);

//     let score = evaluator(&result);
//     tracing::info!("score: {}", score);

//     Some(Best { tx, result, score })
// }

pub struct Best<T, Score: PartialOrd + Ord = U256> {
    pub tx: Arc<T>,
    pub result: ResultAndState,
    pub score: Score,
}

// impl<Ef, C, B> EvmPool<Ef, C, B>
// where
//     Ef: for<'a> EvmFactory<'a> + Send + 'static,
//     C: Cfg + 'static,
//     B: Block + 'static,
// {
//     /// Spawn a task that will evaluate the best candidate from a channel of
//     /// candidates.
//     pub fn spawn<T>(
//         self,
//         mut inbound_tx: UnboundedReceiver<Arc<T>>,
//         evaluator: Box<dyn Fn(&ResultAndState) -> U256 + Send + Sync>,
//         deadline: tokio::time::Instant,
//     ) -> tokio::task::JoinHandle<Option<Best<T>>>
//     where
//         T: Tx + 'static,
//     {
//         tokio::spawn(async move {
//             let mut futs = JoinSet::new();
//             let sleep = tokio::time::sleep_until(deadline);
//             tokio::pin!(sleep);

//             let mut best: Option<Best<T>> = None;

//             loop {
//                 tokio::select! {
//                     biased;
//                     _ = &mut sleep => {
//                         tracing::info!("simulation deadline exceeded");
//                         break
//                     },
//                     tx = inbound_tx.recv() => {
//                         let tx = match tx {
//                             Some(tx) => tx,
//                             None => break,
//                         };

//                         tracing::info!("receiving transaction");

//                         let evm = self.weak_evm();
//                         let eval = evaluator;
//                         let eval = Arc::new(evaluator.as_ref());
//                     }
//                     Some(Ok(Some(candidate))) = futs.join_next() => {
//                         tracing::info!("candidate used gas: {:?}", candidate.result.result.gas_used());
//                         // TODO: think about equality statement here.
//                         // if using ">" then no candidate will be returned if every score is zero
//                         // if using ">=" then the candidate will be replaced every time if every score is zero
//                         if candidate.score >= best.as_ref().map(|b| b.score).unwrap_or_default() {
//                             best = Some(candidate);
//                         }
//                     }
//                 }
//             }
//             best
//         })
//     }
// }

/// SimBlock wraps an array of SimBundles
pub struct SimBlock(Vec<SimBundle>);

///
/// Simulator Factory
///

/// Binds a database and a simulation extension together
#[derive(Clone)]
pub struct SimulatorFactory<Db, Ext> {
    pub db: Db,
    pub ext: Ext,
}

type EvalFn = Arc<dyn Fn(&ResultAndState) -> U256 + Send + Sync>;


/// Creates a new SimulatorFactory from the given Database and Extension
impl<Db, Ext> SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Send + Sync + 'static,
{
    pub fn new(db: Db, ext: Ext) -> Self {
        Self { db, ext }
    }

    /// Spawns a trevm simulator
    pub fn spawn_trevm<T, F>(
        self,
        mut inbound_tx: UnboundedReceiver<Arc<T>>,
        mut inbound_bundle: UnboundedReceiver<Arc<SimBundle>>,
        deadline: tokio::time::Instant,
    ) -> tokio::task::JoinHandle<Option<Best<SimBlock>>>
    where
        T: Tx + Send + Sync + 'static,
        Db: Send + Sync + 'static,
    {
        tokio::spawn(async move {
            let evaluator: Arc<dyn Fn(&ResultAndState) -> U256 + Send + Sync> = Arc::new(|result| {
                // ... your logic ...
                U256::from(1)
            });


            // let mut futs = JoinSet::new();
            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            let mut best: Option<Best<SimBlock>> = None;

            let mut extractor = SimulatorExtractor {
                db: self.db.clone(),
            };

            let t = extractor.trevm(self.db);

            loop {
                select! {
                    _ = sleep => {
                        break;
                    },
                    tx = inbound_tx.recv() => {
                        let tx = match tx {
                            Some(tx) => tx,
                            None => break,
                        };
                        let trevm = extractor.trevm();
                        tracing::info(tx = ?tx);
                        todo!();
                    },
                    b = inbound_bundle.recv() => {
                        todo!();
                    }
                }
            }

            best
        })
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
        let cache: CacheDB<Db> = CacheDB::new(self.db.clone());
        let concurrent_db = ConcurrentState::new(cache, ConcurrentStateInfo::default());
        tracing::info!("created concurrent database");
        Ok(concurrent_db)
    }
}

impl<'a, Db, Ext> EvmFactory<'a> for SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Sync + Send + 'static,
    Ext: Sync + Clone,
{
    type Ext = ();

    /// Create makes a [`ConcurrentState`] database by calling connect
    fn create(&'a self) -> Result<trevm::EvmNeedsCfg<'a, Self::Ext, Self::Database>, Self::Error> {
        let concurrent_db = self.connect()?;
        let t = trevm::revm::EvmBuilder::default().with_db(concurrent_db).build_trevm();
        tracing::info!("created trevm");
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
#[derive(Clone)]
pub struct SimulatorExtractor<Db> {
    db: Db,
}

/// SimulatorExtractor implements a block extractor and trevm block driver
/// for simulating and successively applying state updates from transactions.
impl<Db> BlockExtractor<(), Db> for SimulatorExtractor<Db>
where
    Db: Database + DatabaseCommit + Send + Sync + 'static,
{
    type Driver = SimBundle;

    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, (), Db> {
        trevm::revm::EvmBuilder::default().with_db(db).build_trevm().fill_cfg(&NoopCfg)
    }

    fn extract(&mut self, bytes: &[u8]) -> Self::Driver {
        // TODO: Should this use SimBundle instead of Vec<TxEnvelope>?
        #[allow(clippy::useless_asref)]
        let txs: Vec<TxEnvelope> =
            alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap_or_default();
        SimBundle(txs, NoopBlock)
    }
}

pub struct SimBundle(Vec<TxEnvelope>, NoopBlock);

pub struct SimTxEnvelope(pub TxEnvelope);

impl SimTxEnvelope {
    /// Converts bytes into a SimTxEnvelope
    pub fn to_tx(bytes: &[u8]) -> Option<Self> {
        let tx: TxEnvelope = alloy_rlp::Decodable::decode(&mut bytes.as_ref()).ok()?;
        Some(SimTxEnvelope(tx))
    }

    /// Converts a SimTxEnvelope into bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.0.encode(&mut out);
        out
    }
}

impl From<&[u8]> for SimTxEnvelope {
    fn from(bytes: &[u8]) -> Self {
        let tx: TxEnvelope = alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap();
        SimTxEnvelope(tx)
    }
}

impl From<&SimTxEnvelope> for Vec<u8> {
    fn from(tx: &SimTxEnvelope) -> Self {
        let mut out = Vec::new();
        tx.0.encode(&mut out);
        out
    }
}

impl Tx for SimTxEnvelope {
    fn fill_tx_env(&self, tx_env: &mut revm::primitives::TxEnv) {
        tracing::info!("fillng tx env {:?}", tx_env); // Possible cause
        let revm::primitives::TxEnv { .. } = tx_env;
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
