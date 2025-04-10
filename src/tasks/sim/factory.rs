use crate::tasks::sim::Best;
use alloy::primitives::U256;
use signet_bundle::{SignetEthBundle, SignetEthBundleDriver};
use signet_evm::SignetLayered;
use signet_types::config::SignetSystemConstants;
use std::{convert::Infallible, marker::PhantomData, thread};
use trevm::{
    Block, BundleDriver, Cfg, DbConnect, EvmFactory, EvmNeedsTx, Tx,
    db::cow::CacheOnWrite,
    helpers::Ctx,
    inspectors::{Layered, TimeLimit},
    revm::{DatabaseRef, Inspector, context::result::ResultAndState, inspector::NoOpInspector},
};

/// Factory for creating simulation tasks.
#[derive(Debug, Clone)]
pub struct SimFactory<Db, Insp = NoOpInspector> {
    /// The database to use for the simulation.
    db: Db,

    /// The system constants for the Signet network.
    constants: SignetSystemConstants,

    /// The max time to spend on any simulation.
    execution_timeout: std::time::Duration,

    _pd: PhantomData<fn() -> Insp>,
}

impl<Db, Insp> SimFactory<Db, Insp> {
    /// Creates a new `SimFactory` instance.
    pub fn new(
        db: Db,
        constants: SignetSystemConstants,
        execution_timeout: std::time::Duration,
    ) -> Self {
        Self { db, constants, execution_timeout, _pd: PhantomData }
    }
}

impl<Db, Insp> DbConnect for SimFactory<Db, Insp>
where
    Db: DatabaseRef + Clone + Sync,
    Insp: Sync,
{
    type Database = CacheOnWrite<Db>;

    type Error = Infallible;

    fn connect(&self) -> Result<Self::Database, Self::Error> {
        Ok(CacheOnWrite::new(self.db.clone()))
    }
}

impl<Db, Insp> EvmFactory for SimFactory<Db, Insp>
where
    Db: DatabaseRef + Clone + Sync,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    type Insp = SignetLayered<Layered<TimeLimit, Insp>>;

    fn create(&self) -> Result<trevm::EvmNeedsCfg<Self::Database, Self::Insp>, Self::Error> {
        let db = self.connect().unwrap();

        let inspector = Layered::new(TimeLimit::new(self.execution_timeout), Insp::default());

        Ok(signet_evm::signet_evm_with_inspector(db, inspector, self.constants.clone()))
    }
}

impl<Db, Insp> SimFactory<Db, Insp>
where
    Db: DatabaseRef + Clone + Sync,
    Db::Error: Send,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    /// Simulates a transaction in the context of a block.
    ///
    /// This function runs the simulation in a separate thread and waits for
    /// the result or the deadline to expire.
    pub fn simulate_tx<C: Cfg, B: Block, T: Tx>(
        &self,
        cfg: &C,
        block: &B,
        transaction: T,
        eval: impl Fn(&ResultAndState) -> U256 + Send + Sync,
    ) -> Option<Best<T, ResultAndState>> {
        thread::scope(|s| {
            // simulation thread
            let jh = s.spawn(|| {
                let result = match self.run(cfg, block, &transaction) {
                    Ok(result) => result,
                    Err(e) => return Err(e),
                };

                Ok(Best::new(transaction, result, &eval))
            });
            jh.join()
        })
        .unwrap() // propagate inner panics
        .ok()
    }

    /// Simulates a bundle in the context of a block.
    pub fn simulate_bundle<C, B, T>(
        &self,
        cfg: &C,
        block: &B,
        bundle: SignetEthBundle,
        deadline: std::time::Instant,
        eval: impl FnOnce(
            &EvmNeedsTx<<Self as DbConnect>::Database, <Self as EvmFactory>::Insp>,
        ) -> U256,
    ) -> Option<Best<SignetEthBundle, ()>>
    where
        C: Cfg,
        B: Block,
        Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
    {
        thread::scope(|s| {
            let jh = s.spawn(|| {
                let mut driver = SignetEthBundleDriver::new(
                    &bundle,
                    std::time::Instant::now() + self.execution_timeout,
                );

                let trevm = self.create_with_block(cfg, block).unwrap();

                // run the bundle
                match driver.run_bundle(trevm) {
                    Ok(result) => result,
                    Err(e) => return Err(e.into_error()),
                };

                // evaluate the result
                let score = U256::ZERO;

                Ok(Best::new_unchecked(bundle, (), score))
            });
            jh.join()
        })
        .unwrap() // propagate inner panics
        .ok()
    }
}
