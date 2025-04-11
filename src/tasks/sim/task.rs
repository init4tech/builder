use crate::tasks::sim::{BundleOrTx, SimEnv, SimOutcome};
use alloy::{consensus::TxEnvelope, primitives::U256};
use signet_bundle::SignetEthBundle;
use std::{collections::BTreeMap, num::NonZero, thread};
use tokio::{
    select,
    sync::{
        mpsc::{UnboundedReceiver, UnboundedSender},
        oneshot,
    },
};
use trevm::{
    Block, Cfg,
    db::{TryCachingDb, cow::CacheOnWrite},
    helpers::Ctx,
    revm::{DatabaseRef, Inspector, inspector::NoOpInspector},
};

/// A task that simulates a transaction in the context of a block.
#[derive(Debug)]
pub struct SimTask<Db, C, B, Insp = NoOpInspector> {
    /// The simulation environment.
    sim: SimEnv<Db, C, B, Insp>,

    /// Stream of inbound transactions to simulate.
    inbound_envelopes: UnboundedReceiver<TxEnvelope>,

    /// Stream of bundles to simulate.
    inbound_bundles: UnboundedReceiver<SignetEthBundle>,

    /// The maximum number of bundles to simulate in parallel.
    concurrency_limit: NonZero<usize>,

    /// Transactions that we may simulate.
    sim_buffer: BTreeMap<u128, BundleOrTx<'static>>,

    /// Outbound bundles to send to the block builder.
    outbound_bundles: UnboundedSender<BundleOrTx<'static>>,

    /// The time limit for the simulation.
    finish_by: std::time::Instant,
}

impl<Db, C, B, Insp> SimTask<Db, C, B, Insp> {
    /// The maximum number of bundles to simulate in parallel.
    fn concurrency_limit(&self) -> usize {
        self.concurrency_limit.get()
    }

    /// Add an item to the simulation buffer, return the unique idx that it was
    /// added at.
    fn queue_for_sim(&mut self, item: BundleOrTx<'static>) -> u128 {
        // Remove the least-valuable item if we have too many
        if self.sim_buffer.len() >= 100 {
            let _ = self.sim_buffer.pop_first();
        }

        // Order by tx fee
        let mut total_tx_fee = item.calculate_total_fee();
        // If we have a collision, prioritize the older item by decrementing the
        // tx fee until we find a free spot.
        while self.sim_buffer.contains_key(&total_tx_fee) {
            total_tx_fee -= 1;
        }
        self.sim_buffer.insert(total_tx_fee, item);
        total_tx_fee
    }

    /// Pull items from the buffer into active simulation.
    fn refresh_active_sim(&mut self, active_sim: &mut BTreeMap<u128, BundleOrTx<'static>>) {
        while active_sim.len() > self.concurrency_limit() {
            let (fee, item) = active_sim.pop_first().expect("checked by while condition");
            self.sim_buffer.insert(fee, item);
        }

        while active_sim.len() < self.concurrency_limit() && !self.sim_buffer.is_empty() {
            let (fee, item) = self.sim_buffer.pop_last().expect("checked by while condition");
            active_sim.insert(fee, item);
        }
    }
}

impl<Db, C, B, Insp> SimTask<Db, C, B, Insp>
where
    Db: DatabaseRef + TryCachingDb + Clone + Sync,
    C: Cfg,
    B: Block,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    /// Simulates `concurrency_limit` bundles in parallel.
    pub fn sim_round<'a>(
        &mut self,
        active_sim: &'a mut BTreeMap<u128, BundleOrTx<'static>>,
    ) -> oneshot::Receiver<Option<(u128, SimOutcome<&'a BundleOrTx<'static>>)>> {
        let (tx, rx) = oneshot::channel();

        self.refresh_active_sim(active_sim);

        thread::scope(|scope| {
            let mut handles = Vec::with_capacity(self.concurrency_limit());

            // Spawn a thread per bundle to simulate.
            for (identifier, item) in active_sim.iter() {
                let jh = scope.spawn(|| (*identifier, self.sim.simulate(item).ok()));
                handles.push(jh);
            }

            let mut best = None;
            let best_score = U256::ZERO;

            for handle in handles {
                // Wait for each thread to finish. Find the best outcome.
                if let (identifier, Some(candidate)) = handle.join().unwrap() {
                    if *candidate.score() > best_score {
                        best = Some((identifier, candidate));
                    }
                }
            }

            let _ = tx.send(best);
        });

        rx
    }

    /// Creates a new simulation task.
    pub async fn task(mut self) -> Self {
        // The active simulation map, this is the set of items that are
        // currently being simulated.
        let mut active_sim = BTreeMap::new();

        let finish_by = tokio::time::sleep_until(self.finish_by.into());
        tokio::pin!(finish_by);

        // If we have no active simulation, do nothing.
        let mut rx = self.sim_round(&mut active_sim);

        loop {
            select! {
                _ = &mut finish_by => {
                    break;
                }
                Some(bundle) = self.inbound_bundles.recv() => {
                    self.queue_for_sim(bundle.into());
                }
                Some(tx) = self.inbound_envelopes.recv() => {
                    self.queue_for_sim(tx.into());
                }
                res = &mut rx => {
                    drop(rx);
                    // If simulation errored, we probably have a persistent
                    // issue.
                    let opt = match res {
                        Ok(opt) => opt,

                        Err(err) => {
                            tracing::error!(%err, "Failed to simulate bundle");
                            break
                        },
                    };
                    // No candidate was found. Reset the sim job and continue.
                    let Some((value, candidate)) = opt else {
                        rx = self.sim_round(&mut active_sim);
                        continue;
                    };

                    // Accept the cache
                    // At this point, all EVMs have shut down and been dropped,
                    // so `try_extend` should work.
                    let cache = candidate.into_output();
                    if let Err(err) = self.sim.accept_cache(cache) {
                        tracing::error!(%err, "Failed to extend cache");
                        break;
                    };

                    // Send the bundle onward to the inprogress block
                    let item = active_sim.remove(&value).expect("checked by sim_round");

                    if self.outbound_bundles.send(item).is_err() {
                        // Downstream consumer is closed, we should stop simulating.
                        break;
                    }

                    rx = self.sim_round(&mut active_sim);
                }
            }
        }

        // If we have any items left in the active simulation, return them to
        // our buffer
        for (fee, item) in active_sim.into_iter() {
            self.sim_buffer.insert(fee, item);
        }
        self
    }
}
