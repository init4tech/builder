use alloy::consensus::TxEnvelope;
use init4_bin_base::deps::tracing::{debug, info};
use signet_sim::SimCache;
use signet_tx_cache::types::TxCacheBundle;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use trevm::revm::context::BlockEnv;

/// Cache task for the block builder.
///
/// This tasks handles the ingestion of transactions and bundles into the cache.
/// It keeps a receiver for the block environment and cleans the cache when
/// the environment changes.
#[derive(Debug)]
pub struct CacheTask {
    /// The channel to receive the block environment.
    env: watch::Receiver<Option<BlockEnv>>,

    /// The channel to receive the transaction bundles.
    bundles: mpsc::UnboundedReceiver<TxCacheBundle>,
    /// The channel to receive the transactions.
    txns: mpsc::UnboundedReceiver<TxEnvelope>,
}

impl CacheTask {
    /// Create a new cache task with the given cache and channels.
    pub const fn new(
        env: watch::Receiver<Option<BlockEnv>>,
        bundles: mpsc::UnboundedReceiver<TxCacheBundle>,
        txns: mpsc::UnboundedReceiver<TxEnvelope>,
    ) -> Self {
        Self { env, bundles, txns }
    }

    async fn task_future(mut self, cache: SimCache) {
        loop {
            let mut basefee = 0;
            tokio::select! {
                biased;
                res = self.env.changed() => {
                    if res.is_err() {
                        debug!("Cache task: env channel closed, exiting");
                        break;
                    }
                    if let Some(env) = self.env.borrow_and_update().as_ref() {
                        basefee = env.basefee;
                        info!(basefee, number = env.number, timestamp = env.timestamp, "block env changed, clearing cache");
                        cache.clean(
                            env.number, env.timestamp
                        );
                    }
                }
                Some(bundle) = self.bundles.recv() => {
                    cache.add_item(bundle.bundle, basefee);
                }
                Some(txn) = self.txns.recv() => {
                    cache.add_item(txn, basefee);
                }
            }
        }
    }

    /// Spawn the cache task.
    pub fn spawn(self) -> (SimCache, JoinHandle<()>) {
        let sim_cache = SimCache::default();
        let c = sim_cache.clone();
        let fut = self.task_future(sim_cache);
        (c, tokio::spawn(fut))
    }
}
