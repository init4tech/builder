use crate::tasks::block::InProgressBlock;
use alloy::consensus::TxEnvelope;
use alloy::primitives::U256;
use eyre::Result;
use std::sync::Arc;
use tokio::{select, sync::mpsc::UnboundedReceiver, task::JoinSet};
use trevm::{
    Cfg, DbConnect, NoopBlock, TrevmBuilder, TrevmBuilderError, Tx,
    db::{
        cow::CacheOnWrite,
        sync::{ConcurrentState, ConcurrentStateInfo},
    },
    helpers::Ctx,
    revm::{
        Database, DatabaseCommit, DatabaseRef, Inspector,
        context::{
            CfgEnv,
            result::{EVMError, ExecutionResult, ResultAndState},
        },
        primitives::address,
        state::Account,
    },
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

/// Binds a database and an inspector together for simulation.
#[derive(Debug, Clone)]
pub struct SimulatorFactory<Db, Insp> {
    /// The inspector
    pub inspector: Insp,
    /// A CacheOnWrite that is cloneable
    pub cow: MakeCow<Db>,
}

/// SimResult is an [`Option`] type that holds a tuple of a transaction and its associated
/// state as a [`Db`] type updates if it was successfully executed.
type SimResult<Db> = Result<Option<(Best<TxEnvelope>, CacheOnWrite<Arc<ConcurrentState<Db>>>)>>;

impl<Db, Insp> SimulatorFactory<Db, Insp>
where
    Insp: Inspector<Ctx<CacheOnWrite<CacheOnWrite<Arc<ConcurrentState<Db>>>>>>
        + Send
        + Sync
        + Clone
        + 'static,
    Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + Clone + 'static,
    MakeCow<Db>: DbConnect<Database = CacheOnWrite<Arc<ConcurrentState<Db>>>>,
{
    /// Creates a new Simulator factory from the provided database and inspector.
    pub fn new(db: Db, inspector: Insp) -> Self {
        let cdb = ConcurrentState::new(db, ConcurrentStateInfo::default());
        let cdb = Arc::new(cdb);
        let cow = MakeCow::new(cdb);

        Self { inspector, cow }
    }

    /// Spawns a trevm simulator that runs until `deadline` is hit.
    /// * Spawn does not guarantee that a thread is finished before the deadline.
    /// * This is intentional, so that it can maximize simulation time before the deadline.
    /// * This function always returns whatever the latest finished in progress block is.
    pub fn spawn<F>(
        self,
        mut inbound_tx: UnboundedReceiver<TxEnvelope>,
        evaluator: Arc<F>,
        deadline: tokio::time::Instant,
    ) -> tokio::task::JoinHandle<InProgressBlock>
    where
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
    {
        tokio::spawn(async move {
            let mut join_set = JoinSet::new();
            let mut block = InProgressBlock::new();

            let sleep = tokio::time::sleep_until(deadline);
            tokio::pin!(sleep);

            loop {
                select! {
                    _ = &mut sleep => {
                        tracing::debug!("deadline reached, stopping simulation");
                        break;
                    },
                    tx = inbound_tx.recv() => {
                        tracing::debug!(tx = ?tx, "received transaction");
                        if let Some(inbound_tx) = tx {
                            let eval = evaluator.clone();
                            let sim = self.clone();
                            let db = self.cow.connect().unwrap();

                            join_set.spawn(async move {
                                let result = sim.simulate_tx(inbound_tx, eval, db.nest());
                                match result {
                                    Ok(Some((best, new_db))) => {
                                        tracing::debug!("simulation completed, attempting to update state");
                                        // TODO: call cow.flatten on the nest instead
                                        tracing::debug!("successfully merged simulation state");
                                        return Some(best);
                                    }
                                    Ok(None) => {
                                        tracing::debug!("simulation returned no result");
                                        return None;
                                    }
                                    Err(e) => {
                                        tracing::error!(e = ?e, "failed to simulate transaction");
                                        return None;
                                    }
                                }
                            });
                        }
                    }
                    Some(result) = join_set.join_next() => {
                        println!("join_set result");
                        match result {
                            Ok(Some(best)) => {
                                println!("simulation completed");
                                block.ingest_tx(best.tx.as_ref());
                            },
                            Ok(None) => {
                                println!("simulation returned no result");
                                tracing::debug!("simulation returned no result");
                            }
                            Err(e) => {
                                println!("simulation returned an error: {}", e);
                                tracing::error!(e = ?e, "failed to simulate transaction");
                            }
                        }
                    }
                }
            }

            block
        })
    }

    /// Simulates an inbound tx and applies its state if it's successfully simualted
    pub fn simulate_tx<F>(
        &self,
        tx: TxEnvelope,
        evaluator: Arc<F>,
        db: CacheOnWrite<CacheOnWrite<Arc<ConcurrentState<Db>>>>,
    ) -> SimResult<Db>
    where
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
        Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + Clone + 'static,
    {
        let t = TrevmBuilder::new().with_db(db).with_insp(self.inspector.clone()).build_trevm()?;

        let result = t.fill_cfg(&PecorinoCfg).fill_block(&NoopBlock).fill_tx(&tx).run();

        match result {
            Ok(t) => {
                let result = t.result_and_state().clone();
                let db = t.into_db();
                let score = evaluator(&result);
                let best = Best { tx: Arc::new(tx), result, score };

                // Flatten to save the result to the parent and return it
                let db = db.flatten();

                Ok(Some((best, db)))
            }
            Err(terr) => {
                tracing::error!(err = ?terr.error(), "transaction simulation error");
                Ok(None)
            }
        }
    }

    /// Simulates an inbound bundle and applies its state if it's successfully simulated
    pub fn simulate_bundle<T, F>(
        &self,
        _bundle: Arc<Vec<T>>,
        _evaluator: Arc<F>,
        _db: ConcurrentState<Arc<ConcurrentState<Db>>>,
    ) -> Option<Best<Vec<T>>>
    where
        T: Tx + Send + Sync + 'static,
        F: Fn(&ResultAndState) -> U256 + Send + Sync + 'static,
    {
        todo!("implement bundle handling")
    }
}

/// MakeCow wraps a ConcurrentState database in an Arc to allow for cloning.
#[derive(Debug, Clone)]
pub struct MakeCow<Db>(Arc<ConcurrentState<Db>>);

impl<Db> MakeCow<Db>
where
    Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + 'static,
{
    /// Returns a new CoW Db that implements Clone for use in DbConnect
    pub fn new(db: Arc<ConcurrentState<Db>>) -> Self {
        Self(db)
    }
}

impl<Db> DbConnect for MakeCow<Db>
where
    Db: Database + DatabaseRef + DatabaseCommit + Sync + Send + Clone + 'static,
{
    type Database = CacheOnWrite<Arc<ConcurrentState<Db>>>;
    type Error = TrevmBuilderError;

    /// Connects to the database and returns a CacheOnWrite instance
    fn connect(&self) -> Result<Self::Database, Self::Error> {
        let db: CacheOnWrite<Arc<ConcurrentState<Db>>> = CacheOnWrite::new(self.0.clone());
        Ok(db)
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

/// A simple evaluation function as a sane default.
pub fn eval_fn(state: &ResultAndState) -> U256 {
    // log the transaction results
    match &state.result {
        ExecutionResult::Success { .. } => println!("Execution was successful."),
        ExecutionResult::Revert { .. } => println!("Execution reverted."),
        ExecutionResult::Halt { .. } => println!("Execution halted."),
    }

    // return the target account balance
    let target_addr = address!("0x0000000000000000000000000000000000000000");
    let default_account = Account::default();
    let target_account = state.state.get(&target_addr).unwrap_or(&default_account);
    tracing::info!(balance = ?target_account.info.balance, "target account balance");

    target_account.info.balance
}
