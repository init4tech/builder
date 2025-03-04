use alloy::consensus::TxEnvelope;
use alloy::primitives::U256;
use revm::{db::CacheDB, DatabaseRef};
use std::{convert::Infallible, sync::Arc};
use tokio::{select, sync::mpsc::UnboundedReceiver, task::JoinSet};

use trevm::{
    self,
    db::sync::{ConcurrentState, ConcurrentStateInfo},
    revm::{
        primitives::{EVMError, ResultAndState},
        Database, DatabaseCommit, EvmBuilder,
    },
    BlockDriver, DbConnect, EvmFactory, NoopBlock, NoopCfg, TrevmBuilder, Tx,
};

pub struct Best<T, Score: PartialOrd + Ord = U256> {
    pub tx: Arc<T>,
    pub result: ResultAndState,
    pub score: Score,
}

/// SimBlock wraps an array of SimBundles
pub struct SimBlock(pub Vec<SimBundle>);

/// Binds a database and a simulation extension together
#[derive(Clone)]
pub struct SimulatorFactory<Db, Ext> {
    pub db: Db,
    pub ext: Ext,
}

impl<'a, Db, Ext> SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Send + Sync + 'static,
{
    pub fn new(db: Db, ext: Ext) -> Self {
        Self { db, ext }
    }

    /// Spawns a trevm simulator
    pub fn spawn<T, F>(
        self,
        mut inbound_tx: UnboundedReceiver<Arc<SimTxEnvelope>>,
        _inbound_bundle: UnboundedReceiver<Arc<SimBundle>>,
        evaluator: Arc<F>,
        deadline: tokio::time::Instant,
    ) -> tokio::task::JoinHandle<Option<Best<SimTxEnvelope>>>
    where
        T: Tx,
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
        Ext: Send + Sync + Clone + 'static,
        Db: Database,
        <Db as DatabaseRef>::Error: Send,
    {
        let jh = tokio::spawn(async move {
            let mut best: Option<Best<SimTxEnvelope>> = None;
            let mut join_set = JoinSet::new();

            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            loop {
                select! {
                    _ = &mut sleep => break,
                    tx = inbound_tx.recv() => {
                        if let Some(inbound_tx) = tx {
                            let eval = evaluator.clone();
                            let sim = self.clone();

                            let parent_db =
                                Arc::new(ConcurrentState::new(self.db.clone(), ConcurrentStateInfo::default()));

                            join_set.spawn(async move {
                                sim.simulate_tx(inbound_tx, eval, parent_db)
                            });
                        }
                    }
                    Some(Ok(Some(candidate))) = join_set.join_next() => {
                        tracing::debug!(score = ?candidate.0.score, "job finished");
                        let (candidate, _) = candidate;

                        if candidate.score > best.as_ref().map(|b| b.score).unwrap_or_default() {
                            println!("best score found: {}", candidate.score);
                            best = Some(candidate);
                        }
                    }
                    else => break,
                }
            }

            best
        });

        jh
    }

    /// Simulates an inbound tx and applies its state if it's successfully simualted
    pub fn simulate_tx<F>(
        self,
        tx: Arc<SimTxEnvelope>,
        evaluator: Arc<F>,
        parent_db: Arc<ConcurrentState<Db>>,
    ) -> Option<(Best<SimTxEnvelope>, ConcurrentState<Arc<ConcurrentState<Db>>>)>
    where
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
    {
        let child_db = parent_db.child();
        let trevm_instance = EvmBuilder::default().with_db(child_db).build_trevm();

        let result = trevm_instance
            .fill_cfg(&NoopCfg)
            .fill_block(&NoopBlock)
            .fill_tx(tx.as_ref()) // Use as_ref() to get &SimTxEnvelope from Arc
            .run();

        match result {
            Ok(success) => {
                let score = evaluator(success.result_and_state());
                let result_and_state = success.result_and_state().clone();
                let updated_db = success.into_db();
                Some((Best { tx, result: result_and_state, score }, updated_db))
            }
            Err(e) => {
                tracing::error!("Failed to run transaction: {:?}", e);
                None
            }
        }
    }

    /// Simulates an inbound bundle and applies its state if it's successfully simulated
    pub fn simulate_bundle<T: Tx, F, D>(
        &self,
        _bundle: Arc<Vec<T>>,
        _evaluator: Arc<F>,
        _trevm_instance: trevm::EvmNeedsCfg<'_, (), ConcurrentState<CacheDB<Db>>>,
    ) -> Option<Best<SimBundle>> {
        println!("received tx");
        todo!("implement bundle simulation")
    }
}

// Wraps a Db into an EvmFactory compatible [`Database`]
impl<'a, Db, Ext> DbConnect<'a> for SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Clone + Sync + Send + 'static,
    Ext: Sync + Clone,
{
    type Database = ConcurrentState<CacheDB<Db>>;
    type Error = Infallible;

    fn connect(&'a self) -> Result<Self::Database, Self::Error> {
        let cache: CacheDB<Db> = CacheDB::new(self.db.clone());
        let concurrent_db = ConcurrentState::new(cache, ConcurrentStateInfo::default());
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
        let trevm_instance =
            trevm::revm::EvmBuilder::default().with_db(concurrent_db).build_trevm();
        Ok(trevm_instance)
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
pub struct SimulatorExtractor {}

/// SimulatorExtractor implements a block extractor and trevm block driver
/// for simulating and successively applying state updates from transactions.
impl<Db> BlockExtractor<(), Db> for SimulatorExtractor
where
    Db: Database + DatabaseCommit + Send + Sync + 'static,
{
    type Driver = SimBundle;

    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, (), Db> {
        trevm::revm::EvmBuilder::default().with_db(db).build_trevm().fill_cfg(&NoopCfg)
    }

    fn extract(&mut self, bytes: &[u8]) -> Self::Driver {
        #[allow(clippy::useless_asref)]
        let txs: Vec<TxEnvelope> =
            alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap_or_default();
        let sim_txs = txs.iter().map(|f| SimTxEnvelope(f.clone())).collect();
        SimBundle::new(sim_txs)
    }
}

#[derive(Clone)]
pub struct SimBundle {
    pub transactions: Vec<SimTxEnvelope>,
    pub block: NoopBlock,
}

impl SimBundle {
    pub fn new(transactions: Vec<SimTxEnvelope>) -> Self {
        Self { transactions, block: NoopBlock }
    }
}

#[derive(Clone)]
pub struct SimTxEnvelope(pub TxEnvelope);

impl From<&[u8]> for SimTxEnvelope {
    fn from(bytes: &[u8]) -> Self {
        let tx: TxEnvelope = alloy_rlp::Decodable::decode(&mut bytes.as_ref()).unwrap();
        SimTxEnvelope(tx)
    }
}

impl From<SimTxEnvelope> for Vec<u8> {
    fn from(tx: SimTxEnvelope) -> Vec<u8> {
        alloy_rlp::encode(tx.0)
    }
}

impl Tx for SimTxEnvelope {
    fn fill_tx_env(&self, tx_env: &mut revm::primitives::TxEnv) {
        tracing::info!("fillng tx env {:?}", tx_env);
        let revm::primitives::TxEnv { .. } = tx_env;
    }
}

impl<Ext> BlockDriver<Ext> for SimBundle {
    type Block = NoopBlock;
    type Error<Db: Database + DatabaseCommit> = Error<Db>;

    fn block(&self) -> &Self::Block {
        &self.block
    }

    fn run_txns<'a, Db: Database + DatabaseCommit>(
        &mut self,
        mut trevm: trevm::EvmNeedsTx<'a, Ext, Db>,
    ) -> trevm::RunTxResult<'a, Ext, Db, Self> {
        for tx in self.transactions.iter() {
            if tx.0.recover_signer().is_ok() {
                let sim_tx = SimTxEnvelope(tx.0.clone());
                let t = match trevm.run_tx(&sim_tx) {
                    Ok(t) => {
                        print!(
                            "successfully ran transaction - gas used {}",
                            t.result_and_state().result.gas_used()
                        );
                        t
                    }
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
