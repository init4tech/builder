use crate::tasks::block::InProgressBlock;
use alloy::consensus::TxEnvelope;
use alloy::primitives::U256;
use eyre::Result;
use revm::{db::CacheDB, primitives::CfgEnv, DatabaseRef};
use std::{convert::Infallible, sync::Arc};
use tokio::{select, sync::mpsc::{Receiver, UnboundedReceiver}, task::JoinSet};
use trevm::{
    db::sync::{ConcurrentState, ConcurrentStateInfo},
    revm::{
        primitives::{EVMError, ResultAndState},
        Database, DatabaseCommit, EvmBuilder,
    },
    BlockDriver, Cfg, DbConnect, EvmFactory, NoopBlock, TrevmBuilder, Tx,
};

/// Tracks the EVM state, score, and result of an EVM execution.
/// Scores are assigned by the evaluation function, and are Ord
/// or PartialOrd to allow for sorting.
#[derive(Debug, Clone)]
pub struct Best<T, S: PartialOrd + Ord = U256> {
    /// The transaction being executed.
    pub tx: Arc<T>,
    /// The result and state of the execution.
    pub result: ResultAndState,
    /// The score calculated by the evaluation function.
    pub score: S,
}

/// Binds a database and an extension together.
#[derive(Debug, Clone)]
pub struct SimulatorFactory<Db, Ext> {
    /// The database state the execution is carried out on.
    pub db: Db,
    /// The extension, if any, provided to the trevm instance.
    pub ext: Ext,
}

/// SimResult is an [`Option`] type that holds a tuple of a transaction and its associated
/// state as a [`Db`] type updates if it was successfully executed.
type SimResult<Db> = Option<(Best<TxEnvelope>, ConcurrentState<Arc<ConcurrentState<Db>>>)>;

impl<Db, Ext> SimulatorFactory<Db, Ext>
where
    Ext: Send + Sync + Clone + 'static,
    Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + Clone + 'static,
{
    /// Creates a new Simulator factory out of the database and extension.
    pub const fn new(db: Db, ext: Ext) -> Self {
        Self { db, ext }
    }

    /// Spawns a trevm simulator that runs until `deadline` is hit.
    /// * Spawn does not guarantee that a thread is finished before the deadline.
    /// * This is intentional, so that it can maximize simulation time before the deadline.
    /// * This function always returns whatever the latest finished in progress block is.
    pub fn spawn<T, F>(
        self,
        mut inbound_tx: Receiver<Arc<TxEnvelope>>,
        _inbound_bundle: Receiver<Arc<Vec<TxEnvelope>>>,
        evaluator: Arc<F>,
        deadline: tokio::time::Instant,
    ) -> tokio::task::JoinHandle<InProgressBlock>
    where
        T: Tx,
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
    {
        tokio::spawn(async move {
            // Spawn a join set to track all simulation threads
            let mut join_set = JoinSet::new();

            let mut best: Option<Best<TxEnvelope>> = None;

            let mut block = InProgressBlock::new();

            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            loop {
                select! {
                    _ = &mut sleep => break,
                    // Handle incoming
                    tx = inbound_tx.recv() => {
                        if let Some(inbound_tx) = tx {
                            // Setup the simulation environment
                            let sim = self.clone();
                            let eval = evaluator.clone();
                            let mut parent_db = Arc::new(sim.connect().unwrap());

                            // Kick off the work in a new thread
                            join_set.spawn(async move {
                                let result = sim.simulate_tx(inbound_tx, eval, parent_db.child());

                                if let Some((best, db)) = result {
                                    if let Ok(()) = parent_db.merge_child(db) {
                                        tracing::debug!("merging updated simulation state");
                                        return Some(best)
                                    }
                                    tracing::error!("failed to update simulation state");
                                    None
                                } else {
                                    None
                                }
                            });
                        }
                    }
                    Some(result) = join_set.join_next() => {
                        match result {
                            Ok(Some(candidate)) => {
                                tracing::info!(tx_hash = ?candidate.tx.tx_hash(), "ingesting transaction");
                                block.ingest_tx(candidate.tx.as_ref());

                                if candidate.score > best.as_ref().map(|b| b.score).unwrap_or_default() {
                                    tracing::info!(score = ?candidate.score, "new best candidate found");
                                    best = Some(candidate);
                                }
                            }
                            Ok(None) => {
                                tracing::debug!("simulation returned no result");
                            }
                            Err(e) => {
                                tracing::error!("simulation task failed: {}", e);
                            }
                        }
                    }
                    else => break,
                }
            }

            block
        })
    }

    /// Simulates an inbound tx and applies its state if it's successfully simualted
    pub fn simulate_tx<F>(
        self,
        tx: Arc<TxEnvelope>,
        evaluator: Arc<F>,
        db: ConcurrentState<Arc<ConcurrentState<Db>>>,
    ) -> SimResult<Db>
    where
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
        Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + Clone + 'static,
    {
        let trevm_instance = EvmBuilder::default().with_db(db).build_trevm();

        let result = trevm_instance
            .fill_cfg(&PecorinoCfg)
            .fill_block(&NoopBlock)
            .fill_tx(tx.as_ref()) // Use as_ref() to get &SimTxEnvelope from Arc
            .run();

        match result {
            Ok(t) => {
                // log and evaluate simulation results
                tracing::info!(tx_hash = ?tx.tx_hash(), "transaction simulated");
                let result = t.result_and_state().clone();
                tracing::debug!(gas_used = &result.result.gas_used(), "gas consumed");
                let score = evaluator(&result);
                tracing::debug!(score = ?score, "transaction evaluated");

                // accept results
                let t = t.accept();
                let db = t.1.into_db();

                // return the updated db with the candidate applied to its state
                Some((Best { tx, result, score }, db))
            }
            Err(e) => {
                // if this transaction fails to run, log the error and return None
                tracing::error!(err = ?e.as_transaction_error(), "failed to simulate tx");
                None
            }
        }
    }

    /// Simulates an inbound bundle and applies its state if it's successfully simulated
    pub fn simulate_bundle<T, F>(
        &self,
        _bundle: Arc<Vec<T>>,
        _evaluator: Arc<F>,
        _trevm_instance: trevm::EvmNeedsCfg<'_, (), ConcurrentState<CacheDB<Arc<Db>>>>,
    ) -> Option<Best<Vec<T>>>
    where
        T: Tx + Send + Sync + 'static,
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
    {
        todo!("implement bundle handling")
    }
}

/// Wraps a Db into an EvmFactory compatible [`Database`]
impl<'a, Db, Ext> DbConnect<'a> for SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Sync + Send + Clone + 'static,
    Ext: Sync + Clone,
{
    type Database = ConcurrentState<Db>;
    type Error = Infallible;

    fn connect(&'a self) -> Result<Self::Database, Self::Error> {
        let inner = ConcurrentState::new(self.db.clone(), ConcurrentStateInfo::default());
        Ok(inner)
    }
}

/// Makes a SimulatorFactory capable of creating and configuring trevm instances
impl<'a, Db, Ext> EvmFactory<'a> for SimulatorFactory<Db, Ext>
where
    Db: Database + DatabaseRef + DatabaseCommit + Sync + Send + Clone + 'static,
    Ext: Sync + Clone,
{
    type Ext = ();

    /// Create makes a [`ConcurrentState`] database by calling connect
    fn create(&'a self) -> Result<trevm::EvmNeedsCfg<'a, Self::Ext, Self::Database>, Self::Error> {
        let db = self.connect()?;
        let trevm = trevm::revm::EvmBuilder::default().with_db(db).build_trevm();
        Ok(trevm)
    }
}

/// A trait for extracting transactions from
pub trait BlockExtractor<Ext, Db: Database + DatabaseCommit>: Send + Sync + 'static {
    /// BlockDriver runs the transactions over the provided trevm instance.
    type Driver: BlockDriver<Ext, Error<Db>: core::error::Error>;

    /// Instantiate an configure a new [`trevm`] instance.
    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, Ext, Db>;

    /// Extracts transactions from the source.
    ///
    /// Extraction is infallible. Worst case it should return a no-op driver.
    fn extract(&mut self, bytes: &[u8]) -> Self::Driver;
}

impl<Ext> BlockDriver<Ext> for InProgressBlock {
    type Block = NoopBlock;

    type Error<Db: Database + DatabaseCommit> = Error<Db>;

    fn block(&self) -> &Self::Block {
        &NoopBlock
    }

    /// Loops through the transactions in the block and runs them, accepting the state at the end
    /// if it was successful and returning and erroring out otherwise.
    fn run_txns<'a, Db: Database + DatabaseCommit>(
        &mut self,
        mut trevm: trevm::EvmNeedsTx<'a, Ext, Db>,
    ) -> trevm::RunTxResult<'a, Ext, Db, Self> {
        for tx in self.transactions().iter() {
            if tx.recover_signer().is_ok() {
                let sender = tx.recover_signer().unwrap();
                tracing::info!(sender = ?sender, tx_hash = ?tx.tx_hash(), "simulating transaction");

                let t = match trevm.run_tx(tx) {
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

/// Defines the CfgEnv for Pecorino Network
#[derive(Debug, Clone, Copy)]
pub struct PecorinoCfg;

impl Cfg for PecorinoCfg {
    fn fill_cfg_env(&self, cfg_env: &mut CfgEnv) {
        cfg_env.chain_id = 17003;
    }
}

/// Wrap the EVM error in a database error type
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
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Error")
    }
}

impl<Db: Database> core::fmt::Display for Error<Db> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Error")
    }
}
