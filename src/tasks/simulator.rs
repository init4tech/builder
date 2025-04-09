// use crate::tasks::{block::InProgressBlock, sim::Best};
// use alloy::consensus::TxEnvelope;
// use alloy::primitives::U256;
// use eyre::Result;
// use std::sync::Arc;
// use tokio::{select, sync::mpsc::UnboundedReceiver, task::JoinSet};
// use trevm::{
//     Cfg, DbConnect, NoopBlock, TrevmBuilder, TrevmBuilderError, Tx,
//     db::{
//         cow::CacheOnWrite,
//         sync::{ConcurrentState, ConcurrentStateInfo},
//     },
//     helpers::Ctx,
//     revm::{
//         Database, DatabaseCommit, DatabaseRef, Inspector,
//         context::{
//             CfgEnv,
//             result::{EVMError, ExecutionResult, ResultAndState},
//         },
//         primitives::address,
//         state::Account,
//     },
// };

// /// Binds a database and an inspector together for simulation.
// #[derive(Debug, Clone)]
// pub struct SimulatorFactory<Db, Insp> {
//     /// The inspector
//     pub inspector: Insp,
//     /// A CacheOnWrite that is cloneable
//     pub cow: MakeCow<Db>,
// }

// impl<Db, Insp> SimulatorFactory<Db, Insp>
// where
//     Insp: Inspector<Ctx<CacheOnWrite<CacheOnWrite<Arc<ConcurrentState<Db>>>>>>
//         + Send
//         + Sync
//         + Clone
//         + 'static,
//     Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + Clone + 'static,
//     MakeCow<Db>: DbConnect<Database = CacheOnWrite<Arc<ConcurrentState<Db>>>>,
// {
//     /// Creates a new Simulator factory from the provided database and inspector.
//     pub fn new(db: Db, inspector: Insp) -> Self {
//         let cdb = ConcurrentState::new(db, ConcurrentStateInfo::default());
//         let cdb = Arc::new(cdb);
//         let cow = MakeCow::new(cdb);

//         Self { inspector, cow }
//     }
// }

// /// MakeCow wraps a ConcurrentState database in an Arc to allow for cloning.
// #[derive(Debug, Clone)]
// pub struct MakeCow<Db>(Arc<ConcurrentState<Db>>);

// impl<Db> MakeCow<Db>
// where
//     Db: Database + DatabaseRef + DatabaseCommit + Send + Sync + 'static,
// {
//     /// Returns a new CoW Db that implements Clone for use in DbConnect
//     pub fn new(db: Arc<ConcurrentState<Db>>) -> Self {
//         Self(db)
//     }
// }

// impl<Db> DbConnect for MakeCow<Db>
// where
//     Db: Database + DatabaseRef + DatabaseCommit + Sync + Send + Clone + 'static,
// {
//     type Database = CacheOnWrite<Arc<ConcurrentState<Db>>>;
//     type Error = TrevmBuilderError;

//     /// Connects to the database and returns a CacheOnWrite instance
//     fn connect(&self) -> Result<Self::Database, Self::Error> {
//         let db: CacheOnWrite<Arc<ConcurrentState<Db>>> = CacheOnWrite::new(self.0.clone());
//         Ok(db)
//     }
// }

// /// Defines the CfgEnv for Pecorino Network
// #[derive(Debug, Clone, Copy)]
// pub struct PecorinoCfg;

// impl Cfg for PecorinoCfg {
//     fn fill_cfg_env(&self, cfg_env: &mut CfgEnv) {
//         cfg_env.chain_id = 17003;
//     }
// }

// /// Wrap the EVM error in a database error type
// pub struct Error<Db: Database>(EVMError<Db::Error>);

// impl<Db> From<EVMError<Db::Error>> for Error<Db>
// where
//     Db: Database,
// {
//     fn from(e: EVMError<Db::Error>) -> Self {
//         Self(e)
//     }
// }

// impl<Db: Database> core::error::Error for Error<Db> {}

// impl<Db: Database> core::fmt::Debug for Error<Db> {
//     fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
//         write!(f, "Error")
//     }
// }

// impl<Db: Database> core::fmt::Display for Error<Db> {
//     fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
//         write!(f, "Error")
//     }
// }

// /// A simple evaluation function as a sane default.
// pub fn eval_fn(state: &ResultAndState) -> U256 {
//     // log the transaction results
//     match &state.result {
//         ExecutionResult::Success { .. } => println!("Execution was successful."),
//         ExecutionResult::Revert { .. } => println!("Execution reverted."),
//         ExecutionResult::Halt { .. } => println!("Execution halted."),
//     }

//     // return the target account balance
//     let target_addr = address!("0x0000000000000000000000000000000000000000");
//     let default_account = Account::default();
//     let target_account = state.state.get(&target_addr).unwrap_or(&default_account);
//     tracing::info!(balance = ?target_account.info.balance, "target account balance");

//     target_account.info.balance
// }
