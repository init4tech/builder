use crate::tasks::block::InProgressBlock;
use alloy::consensus::TxEnvelope;
use alloy::primitives::U256;
use eyre::Result;
use std::sync::Arc;
use tokio::{select, sync::mpsc::UnboundedReceiver, task::JoinSet};
use trevm::{
    db::sync::{ConcurrentState, ConcurrentStateInfo}, helpers::Ctx, revm::{
        context::{
            result::{EVMError, ExecutionResult, ResultAndState}, CfgEnv
        }, inspector::inspectors::GasInspector, primitives::address, state::Account, Database, DatabaseCommit, DatabaseRef, Inspector
    }, BlockDriver, Cfg, DbConnect, EvmFactory, NoopBlock, Trevm, TrevmBuilder, TrevmBuilderError, Tx
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
pub struct SimulatorFactory<Db, Insp> {
    /// The database state the execution is carried out on.
    pub db: Db,
    /// The inspector
    pub inspector: Insp,
}

/// SimResult is an [`Option`] type that holds a tuple of a transaction and its associated
/// state as a [`Db`] type updates if it was successfully executed.
type SimResult<Db> = Result<Option<(Best<TxEnvelope>, ConcurrentState<Arc<ConcurrentState<Db>>>)>>;

impl<Db, Insp> SimulatorFactory<Db, Insp>
where
    Insp: Inspector<Ctx<ConcurrentState<Db>>> + Send + Sync + Clone + 'static,
    Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + Clone + 'static,
{
    /// Creates a new Simulator factory out of the database and extension.
    pub const fn new(db: Db, inspector: Insp) -> Self {
        Self { db, inspector }
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
                            let db = self.connect().expect("must connect db");
                            let mut parent_db = Arc::new(db);

                            join_set.spawn(async move {
                                let result = sim.simulate_tx(inbound_tx, eval, parent_db.child());

                                if let Ok(Some((best, db))) = result {
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
        tx: TxEnvelope,
        evaluator: Arc<F>,
        db: ConcurrentState<Arc<ConcurrentState<Db>>>,
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

impl<Db, Insp> DbConnect for SimulatorFactory<Db, Insp>
where
    Db: Database + DatabaseRef + DatabaseCommit + Sync + Send + Clone + 'static,
    Insp: Inspector<Ctx<ConcurrentState<Db>>> + Sync + Send + Clone,
{
    type Database = ConcurrentState<Db>;
    type Error = TrevmBuilderError;

    fn connect(&self) -> Result<Self::Database, Self::Error> {
        let inner = ConcurrentState::new(self.db.clone(), ConcurrentStateInfo::default());
        Ok(inner)
    }
}

/// Makes a SimulatorFactory capable of creating and configuring trevm instances
impl<Db, Insp> EvmFactory for SimulatorFactory<Db, Insp>
where
    Db: Database + DatabaseRef + DatabaseCommit + Sync + Send + Clone + 'static,
    Insp: Inspector<Ctx<ConcurrentState<Db>>> + Sync + Send + Clone,
{
    type Insp = Insp;

    fn create(
        &self,
    ) -> std::result::Result<trevm::EvmNeedsCfg<Self::Database, Self::Insp>, Self::Error> {
        let db = self.connect()?;
        let result =
            TrevmBuilder::new().with_db(db).with_insp(self.inspector.clone()).build_trevm();
        match result {
            Ok(t) => Ok(t),
            Err(e) => Err(e.into()),
        }
    }
}

pub trait BlockExtractor<Insp, Db>
where
    Db: Database + DatabaseCommit,
    Insp: Inspector<Ctx<Db>>,
{
    /// BlockDriver runs the transactions over the provided trevm instance.
    type Driver: BlockDriver<Insp, Error<Db>: core::error::Error>;

    /// Instantiate an configure a new [`trevm`] instance.
    fn trevm(&self, db: Db) -> trevm::EvmNeedsBlock<Db, Insp>;

    /// Extracts transactions from the source.
    ///
    /// Extraction is infallible. Worst case it should return a no-op driver.
    fn extract(&mut self, bytes: &[u8]) -> Self::Driver;
}

impl<Insp> BlockDriver<Insp> for InProgressBlock {
    type Block = NoopBlock;

    type Error<Db: Database + DatabaseCommit> = Error<Db>;

    fn block(&self) -> &Self::Block {
        &NoopBlock
    }

    /// Loops through the transactions in the block and runs them, accepting the state at the end
    /// if it was successful and returning and erroring out otherwise.
    fn run_txns<Db: Database + DatabaseCommit>(
        &mut self,
        mut trevm: trevm::EvmNeedsTx<Db, Insp>,
    ) -> trevm::RunTxResult<Db, Insp, Self>
    where
        Insp: Inspector<Ctx<Db>>,
    {
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
        _trevm: &trevm::EvmNeedsBlock<Db, Insp>,
    ) -> Result<(), Self::Error<Db>>
    where
        Insp: Inspector<Ctx<Db>>,
    {
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
