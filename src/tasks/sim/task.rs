use crate::tasks::sim::{SimEnv, SimOutcome};
use alloy::{
    consensus::{Transaction, TxEnvelope},
    eips::Decodable2718,
    primitives::U256,
};
use signet_bundle::SignetEthBundle;
use std::collections::BTreeMap;
use tokio::{select, sync::mpsc::UnboundedReceiver};
use trevm::{
    Block, Cfg,
    db::cow::CacheOnWrite,
    helpers::Ctx,
    revm::{DatabaseRef, Inspector, inspector::NoOpInspector},
};

/// Either a bundle or a transaction.
#[derive(Debug, Clone)]
enum BundleOrTx {
    Bundle(SignetEthBundle),
    Tx(TxEnvelope),
}

impl From<SignetEthBundle> for BundleOrTx {
    fn from(bundle: SignetEthBundle) -> Self {
        Self::Bundle(bundle)
    }
}

impl From<TxEnvelope> for BundleOrTx {
    fn from(tx: TxEnvelope) -> Self {
        Self::Tx(tx)
    }
}

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
    concurrency_limit: usize,
}

impl<Db, C, B, Insp> SimTask<Db, C, B, Insp>
where
    Db: DatabaseRef + Clone + Sync,
    C: Cfg,
    B: Block,
    Insp: Inspector<Ctx<CacheOnWrite<Db>>> + Default + Sync,
{
    /// Simulates `concurrency_limit` bundles in parallel.
    pub fn sim_round<'a, I>(
        &self,
        to_sim: &mut I,
    ) -> tokio::sync::oneshot::Receiver<Option<SimOutcome<&'a BundleOrTx>>>
    where
        I: Iterator<Item = &'a BundleOrTx>,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::scope(|scope| {
            // Set up a container for the handles.
            let mut handles = Vec::with_capacity(self.concurrency_limit);

            // Spawn a thread per bundle to simulate.
            for item in to_sim.take(self.concurrency_limit) {
                let jh = scope.spawn(|| match item {
                    BundleOrTx::Bundle(bundle) => self.sim.simulate_bundle(bundle).ok(),
                    BundleOrTx::Tx(tx) => self.sim.simulate(tx).ok().map(SimOutcome::map_in_into),
                });
                handles.push(jh);
            }

            let mut best = None;
            let best_score = U256::ZERO;

            for handle in handles {
                // Wait for each thread to finish. Find the best outcome.
                let result = handle.join().unwrap();
                if let Some(candidate) = result {
                    if *candidate.score() > best_score {
                        best = Some(candidate);
                    }
                }
            }

            let _ = tx.send(best);
        });
        rx
    }

    /// Creates a new simulation task.
    pub async fn task(mut self) {
        loop {
            let mut buffer: BTreeMap<u128, BundleOrTx> = BTreeMap::new();

            let rx = self.sim_round(&mut buffer.values().rev());

            select! {
                Some(bundle) = self.inbound_bundles.recv() => {
                    // Ensure all txns are valid and calculate the total tx fee
                    let mut total_tx_fee = 0;
                    for tx in bundle.bundle.txs.iter() {
                        let Ok(tx) = TxEnvelope::decode_2718(&mut tx.as_ref()) else {
                            continue;
                        };
                        total_tx_fee += tx.effective_gas_price(None) * tx.gas_limit() as u128;
                    }
                    while buffer.contains_key(&total_tx_fee) {
                        total_tx_fee -= 1;
                    }
                    buffer.insert(total_tx_fee, bundle.into());
                }
                Some(tx) = self.inbound_envelopes.recv() => {
                    // Calculate the total tx fee.
                    let mut tx_fee = tx.effective_gas_price(None) * tx.gas_limit() as u128;
                    while buffer.contains_key(&tx_fee) {
                        tx_fee -= 1;
                    }
                    buffer.insert(tx_fee, tx.into());
                }
            }
        }
    }
}
