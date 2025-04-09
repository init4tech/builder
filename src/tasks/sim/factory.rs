use crate::tasks::sim::Best;
use alloy::primitives::U256;
use signet_bundle::SignetEthBundle;
use signet_evm::SignetLayered;
use signet_types::config::SignetSystemConstants;
use std::{borrow::Borrow, convert::Infallible, marker::PhantomData, sync::Arc};
use tokio::select;
use trevm::{
    Block, Cfg, DbConnect, EvmFactory, Tx,
    db::cow::CacheOnWrite,
    helpers::Ctx,
    revm::{DatabaseRef, Inspector, context::result::ResultAndState, inspector::NoOpInspector},
};

/// Factory for creating simulation tasks.
#[derive(Debug, Clone)]
pub struct SimFactory<Db, Insp = NoOpInspector> {
    db: Db,
    constants: SignetSystemConstants,

    _pd: PhantomData<fn() -> Insp>,
}

impl<Db, Insp> SimFactory<Db, Insp> {
    /// Creates a new `SimFactory` instance.
    pub fn new(db: Db, constants: SignetSystemConstants) -> Self {
        Self { db, constants, _pd: PhantomData }
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
    type Insp = SignetLayered<Insp>;

    fn create(&self) -> Result<trevm::EvmNeedsCfg<Self::Database, Self::Insp>, Self::Error> {
        let db = self.connect().unwrap();
        let inspector = Insp::default();
        Ok(signet_evm::signet_evm_with_inspector(db, inspector, self.constants.clone()))
    }
}

impl<Db, Insp> SimFactory<Db, Insp>
where
    Db: DatabaseRef + Clone + Sync,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    /// Simulates a transaction in the context of a block.
    ///
    /// This function runs the simulation in a separate thread and waits for
    /// the result or the deadline to expire.
    pub fn simulate<C: Cfg, B: Block, T: Tx>(
        &self,
        cfg: &C,
        block: &B,
        transaction: T,
        deadline: std::time::Instant,
        eval: impl Fn(&ResultAndState) -> U256 + Send + Sync,
    ) -> Option<Best<T, ResultAndState>> {
        let (sender, mut rx) = tokio::sync::mpsc::channel(1);

        std::thread::scope(|s| {
            // timeout thread
            s.spawn(|| {
                std::thread::sleep(deadline - std::time::Instant::now());
                let _ = sender.blocking_send(None);
            });

            // simulation thread
            s.spawn(|| {
                let Ok(result) = self.run(cfg, block, &transaction) else {
                    let _ = sender.blocking_send(None);
                    return;
                };

                let best = Best::new(transaction, result, &eval);

                let _ = sender.blocking_send(Some(best));
            });

            rx.blocking_recv().flatten()
        })
    }

    fn simulate_bundle<C, B, T>(
        &self,
        cfg: &C,
        block: &B,
        bundle: SignetEthBundle,
        deadline: std::time::Instant,
        eval: impl FnOnce(&ResultAndState) -> U256,
    ) -> Option<Best<SignetEthBundle, ()>> {
        todo!()
    }
}
