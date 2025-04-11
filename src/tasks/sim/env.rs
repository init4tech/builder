use crate::tasks::sim::SimOutcome;
use signet_bundle::{SignetEthBundle, SignetEthBundleDriver, SignetEthBundleError};
use signet_evm::SignetLayered;
use signet_types::config::SignetSystemConstants;
use std::{convert::Infallible, marker::PhantomData};
use trevm::{
    Block, BundleDriver, Cfg, DbConnect, EvmFactory, Tx,
    db::{TryCachingDb, cow::CacheOnWrite},
    helpers::Ctx,
    inspectors::{Layered, TimeLimit},
    revm::{
        DatabaseRef, Inspector, context::result::EVMError, database::Cache,
        inspector::NoOpInspector,
    },
};

use super::BundleOrTx;

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

    /// Get a reference to the database.
    pub fn db_mut(&mut self) -> &mut Db {
        &mut self.db
    }

    /// Get a reference to the system constants.
    pub fn constants(&self) -> &SignetSystemConstants {
        &self.constants
    }

    /// Get a reference to the chain cfg.
    pub fn cfg(&self) -> &C {
        &self.cfg
    }

    /// Get a mutable reference to the chain cfg.
    pub fn cfg_mut(&mut self) -> &mut C {
        &mut self.cfg
    }

    /// Get a reference to the block.
    pub fn block(&self) -> &B {
        &self.block
    }

    /// Get a mutable reference to the block.
    pub fn block_mut(&mut self) -> &mut B {
        &mut self.block
    }

    /// Get the exectuion timeout.
    pub fn execution_timeout(&self) -> std::time::Duration {
        self.execution_timeout
    }

    /// Set the execution timeout.
    pub fn set_execution_timeout(&mut self, timeout: std::time::Duration) {
        self.execution_timeout = timeout;
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
    fn simulate_tx<'a, T>(
        &self,
        transaction: &'a T,
    ) -> Result<SimOutcome<&'a T, Cache>, SignetEthBundleError<CacheOnWrite<Db>>>
    where
        T: Tx,
    {
        let trevm = self.create_with_block(&self.cfg, &self.block).unwrap();

        // Get the initial beneficiary balance
        let beneificiary = trevm.beneficiary();
        let initial_beneficiary_balance =
            trevm.try_read_balance_ref(beneificiary).map_err(EVMError::Database)?;

        // If succesful, take the cache. If failed, return the error.
        match trevm.run_tx(transaction) {
            Ok(trevm) => {
                // Get the beneficiary balance after the transaction and calculate the
                // increase
                let beneficiary_balance =
                    trevm.try_read_balance_ref(beneificiary).map_err(EVMError::Database)?;
                let increase = beneficiary_balance.saturating_sub(initial_beneficiary_balance);

                let cache = trevm.accept_state().into_db().into_cache();

                // Create the outcome
                Ok(SimOutcome::new_unchecked(transaction, cache, increase))
            }
            Err(e) => Err(SignetEthBundleError::from(e.into_error())),
        }
    }

    /// Simulates a bundle in the context of a block.
    fn simulate_bundle<'a>(
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

    /// Simulate a [`BundleOrTx`], containing either a [`SignetEthBundle`] or a
    /// [`TxEnvelope`]
    pub fn simulate<'a, 'b: 'a>(
        &self,
        item: &'a BundleOrTx<'b>,
    ) -> Result<SimOutcome<&'a BundleOrTx<'b>>, SignetEthBundleError<CacheOnWrite<Db>>> {
        match item {
            BundleOrTx::Bundle(bundle) => {
                Ok(self.simulate_bundle(bundle.as_ref())?.map_in(|_| item))
            }
            BundleOrTx::Tx(tx) => Ok(self.simulate_tx(tx.as_ref())?.map_in(|_| item)),
        }
    }
}

impl<Db, C, B, Insp> SimEnv<Db, C, B, Insp>
where
    Db: TryCachingDb + DatabaseRef + Clone + Sync,
    C: Cfg + Sync,
    B: Block + Sync,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    /// Accepts a cache from the simulation and extends the database with it.
    pub fn accept_cache(&mut self, cache: Cache) -> Result<(), <Db as TryCachingDb>::Error> {
        self.db_mut().try_extend(cache)
    }
}
