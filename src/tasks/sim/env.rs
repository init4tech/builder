use crate::tasks::sim::SimOutcome;
use alloy::primitives::U256;
use signet_bundle::{SignetEthBundle, SignetEthBundleDriver, SignetEthBundleError};
use signet_evm::SignetLayered;
use signet_types::config::SignetSystemConstants;
use std::{convert::Infallible, marker::PhantomData};
use trevm::{
    Block, BundleDriver, Cfg, DbConnect, EvmFactory, Tx,
    db::cow::CacheOnWrite,
    helpers::Ctx,
    inspectors::{Layered, TimeLimit},
    revm::{
        DatabaseRef, Inspector, context::result::ResultAndState, database::Cache,
        inspector::NoOpInspector,
    },
};

/// Factory for creating simulation tasks.
#[derive(Debug, Clone)]
pub struct SimEnv<Db, C, B, Insp = NoOpInspector> {
    /// The database to use for the simulation.
    db: Db,

    /// The system constants for the Signet network.
    constants: SignetSystemConstants,

    /// Chain cfg to use for the simulation.
    cfg: C,

    /// Block to use for the simulation.
    block: B,

    /// The max time to spend on any simulation.
    execution_timeout: std::time::Duration,

    _pd: PhantomData<fn() -> Insp>,
}

impl<Db, C, B, Insp> SimEnv<Db, C, B, Insp> {
    /// Creates a new `SimFactory` instance.
    pub fn new(
        db: Db,
        constants: SignetSystemConstants,
        cfg: C,
        block: B,
        execution_timeout: std::time::Duration,
    ) -> Self {
        Self { db, constants, cfg, block, execution_timeout, _pd: PhantomData }
    }
}

impl<Db, C, B, Insp> DbConnect for SimEnv<Db, C, B, Insp>
where
    Db: DatabaseRef + Clone + Sync,
    C: Sync,
    B: Sync,
    Insp: Sync,
{
    type Database = CacheOnWrite<Db>;

    type Error = Infallible;

    fn connect(&self) -> Result<Self::Database, Self::Error> {
        Ok(CacheOnWrite::new(self.db.clone()))
    }
}

impl<Db, C, B, Insp> EvmFactory for SimEnv<Db, C, B, Insp>
where
    Db: DatabaseRef + Clone + Sync,
    C: Sync,
    B: Sync,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    type Insp = SignetLayered<Layered<TimeLimit, Insp>>;

    fn create(&self) -> Result<trevm::EvmNeedsCfg<Self::Database, Self::Insp>, Self::Error> {
        let db = self.connect().unwrap();

        let inspector = Layered::new(TimeLimit::new(self.execution_timeout), Insp::default());

        Ok(signet_evm::signet_evm_with_inspector(db, inspector, self.constants.clone()))
    }
}

impl<Db, C, B, Insp> SimEnv<Db, C, B, Insp>
where
    Db: DatabaseRef + Clone + Sync,
    C: Cfg + Sync,
    B: Block + Sync,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    /// Simulates a transaction in the context of a block.
    ///
    /// This function runs the simulation in a separate thread and waits for
    /// the result or the deadline to expire.
    pub fn simulate<T>(
        &self,
        transaction: T,
        eval: impl Fn(&ResultAndState) -> U256 + Send + Sync,
    ) -> Result<SimOutcome<T, ResultAndState>, SignetEthBundleError<CacheOnWrite<Db>>>
    where
        T: Tx,
    {
        let result = self.run(&self.cfg, &self.block, &transaction)?;
        Ok(SimOutcome::new(transaction, result, &eval))
    }

    /// Simulates a bundle in the context of a block.
    pub fn simulate_bundle<'a>(
        &self,
        bundle: &'a SignetEthBundle,
    ) -> Result<SimOutcome<&'a SignetEthBundle, Cache>, SignetEthBundleError<CacheOnWrite<Db>>>
    where
        Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
    {
        let mut driver =
            SignetEthBundleDriver::new(&bundle, std::time::Instant::now() + self.execution_timeout);
        let trevm = self.create_with_block(&self.cfg, &self.block).unwrap();

        // run the bundle
        let trevm = match driver.run_bundle(trevm) {
            Ok(result) => result,
            Err(e) => return Err(e.into_error()),
        };

        // evaluate the result
        let score = driver.beneficiary_balance_increase();

        let db = trevm.into_db();
        let cache = db.into_cache();

        Ok(SimOutcome::new_unchecked(bundle, cache, score))
    }
}
