use crate::tasks::sim::{SimEnv, SimOutcome};
use alloy::primitives::U256;
use signet_bundle::SignetEthBundle;
use trevm::{
    Block, Cfg, EvmFactory,
    db::cow::CacheOnWrite,
    helpers::Ctx,
    revm::{DatabaseRef, Inspector, database::Cache, inspector::NoOpInspector},
};

/// A task that simulates a transaction in the context of a block.
#[derive(Debug)]
pub struct SimTask<Db, C, B, Insp = NoOpInspector> {
    factory: SimEnv<Db, C, B, Insp>,
    concurrency_limit: usize,
}

impl<Db, C, B, Insp> SimTask<Db, C, B, Insp>
where
    SimEnv<Db, C, B, Insp>: EvmFactory,
    Db: DatabaseRef + Clone + Sync,
    C: Cfg,
    B: Block,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    /// Simulates `concurrency_limit` bundles in parallel.
    pub fn sim_round<'a, I>(
        &self,
        bundles: &mut I,
    ) -> Option<SimOutcome<&'a SignetEthBundle, Cache>>
    where
        I: Iterator<Item = &'a SignetEthBundle>,
    {
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(self.concurrency_limit);

            // Spawn a thread per bundle to simulate.
            for bundle in bundles.take(self.concurrency_limit) {
                let jh = scope.spawn(|| self.factory.simulate_bundle(bundle).ok());
                handles.push(jh);
            }

            let mut best = None;
            let best_score = U256::ZERO;

            for handle in handles {
                let result = handle.join().unwrap();
                if let Some(candidate) = result {
                    if *candidate.score() > best_score {
                        best = Some(candidate);
                    }
                }
            }

            best
        })
    }
}
