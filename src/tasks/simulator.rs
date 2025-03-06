use super::bundler::Bundle;
use crate::tasks::block::InProgressBlock;
use alloy::consensus::TxEnvelope;
use alloy::primitives::U256;
use eyre::Result;
use revm::{db::CacheDB, DatabaseRef};
use std::{convert::Infallible, sync::Arc};
use tokio::{select, sync::mpsc::UnboundedReceiver, task::JoinSet};
use trevm::{
    self,
    db::sync::{Child, ConcurrentState, ConcurrentStateInfo},
    revm::{
        primitives::{EVMError, ResultAndState},
        Database, DatabaseCommit, EvmBuilder,
    },
    BlockDriver, DbConnect, EvmFactory, NoopBlock, NoopCfg, TrevmBuilder, Tx,
};
use zenith_types::ZenithEthBundle;

pub struct Best<T, Score: PartialOrd + Ord = U256> {
    pub tx: Arc<T>,
    pub result: ResultAndState,
    pub score: Score,
}

/// Binds a database and a simulation extension together
#[derive(Clone)]
pub struct SimulatorFactory<Db, Ext> {
    pub db: Db,
    pub ext: Ext,
}

impl<'a, Db, Ext> SimulatorFactory<Db, Ext>
where
    Ext: Send + Sync + Clone + 'static,
    Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + Clone + 'static,
{
    pub fn new(db: Db, ext: Ext) -> Self {
        Self { db, ext }
    }

    /// Spawns a trevm simulator.
    /// Spawn does not guarantee that a thread is finished before the deadline.
    /// This is intentional, so that it can maximize simulation time before the deadline.
    /// This function will always return whatever the latest finished best was.
    pub fn spawn<T, F>(
        self,
        mut inbound_tx: UnboundedReceiver<Arc<SimTxEnvelope>>,
        _inbound_bundle: UnboundedReceiver<Arc<Vec<SimTxEnvelope>>>,
        evaluator: Arc<F>,
        deadline: tokio::time::Instant,
    ) -> tokio::task::JoinHandle<Option<Best<SimTxEnvelope>>>
    where
        T: Tx,
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
    {
        let jh = tokio::spawn(async move {
            // Spawn a join set to track all simulation threads
            let mut join_set = JoinSet::new();

            let mut best: Option<Best<SimTxEnvelope>> = None;

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
                                    if let Ok(()) = parent_db.can_merge(&db) {
                                        if let Ok(()) = parent_db.merge_child(db) {
                                            tracing::info!("merged db");
                                        }
                                    }
                                    Some(best)
                                } else {
                                    None
                                }
                            });
                        }
                    }
                    Some(Ok(Some(candidate))) = join_set.join_next() => {
                        tracing::debug!(score = ?candidate.score, "job finished");
                        if candidate.score > best.as_ref().map(|b| b.score).unwrap_or_default() {
                            tracing::info!(score = ?candidate.score, "new best candidate found");
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
        child_db: Child<Db>,
    ) -> Option<(Best<SimTxEnvelope>, Child<Db>)>
    where
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
        Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + Clone + 'static,
    {
        let trevm_instance = EvmBuilder::default().with_db(child_db).build_trevm();

        let result = trevm_instance
            .fill_cfg(&NoopCfg)
            .fill_block(&NoopBlock)
            .fill_tx(tx.as_ref()) // Use as_ref() to get &SimTxEnvelope from Arc
            .run();

        match result {
            Ok(t) => {
                let hash = tx.0.tx_hash();
                tracing::info!(hash = ?hash, "simulated transaction");

                let res = t.result_and_state();
                let score = evaluator(res);
                tracing::debug!(score = ?score, "evaluated transaction score");

                let result_and_state = res.clone();
                tracing::debug!(gas_used = result_and_state.result.gas_used(), "gas consumed");

                // accept and return the updated_db with the execution score
                let t = t.accept();

                // take the db and return it wth the best
                let db = t.1.into_db();

                Some((Best { tx, result: result_and_state, score }, db))
            }
            Err(e) => {
                tracing::error!("Failed to run transaction: {:?}", e);
                None
            }
        }
    }

    /// Adds a given bundle to a given block, creates a [`trevm`] instance,
    /// runs the  [`BlockDriver`] with that instance, and then returns
    /// the [`Child<Db>`] of the updated state.
    pub fn apply_bundle(
        &self,
        mut block: InProgressBlock,
        bundle: Arc<ZenithEthBundle>,
        child_db: Child<Db>,
    ) -> Result<Child<Db>> {
        let trevm = EvmBuilder::default()
            .with_db(child_db)
            .build_trevm()
            .fill_cfg(&NoopCfg)
            .fill_block(&NoopBlock);

        block.ingest_bundle(Bundle {
            id: bundle.replacement_uuid().unwrap_or_default().to_string(),
            bundle: ZenithEthBundle {
                bundle: bundle.bundle.clone(),
                host_fills: bundle.host_fills.clone(),
            },
        });

        let result = block.run_txns(trevm);
        match result {
            Ok(t) => {
                let db = t.into_db();
                Ok(db)
            }
            Err(t_error) => {
                tracing::error!(err = ?t_error, "Failed to run block");
                eyre::bail!("Failed to run block");
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
    type Driver: BlockDriver<Ext, Error<Db>: core::error::Error>;

    /// Instantiate an configure a new [`trevm`] instance.
    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<'static, Ext, Db>;

    /// Extracts transactions from the source.
    ///
    /// Extraction is infallible. Worst case it should return a no-op driver.
    fn extract(&mut self, bytes: &[u8]) -> Self::Driver;
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
