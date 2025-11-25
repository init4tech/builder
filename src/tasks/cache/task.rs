use crate::tasks::env::SimEnv;
use alloy::consensus::TxEnvelope;
use signet_sim::SimCache;
use signet_tx_cache::types::TxCacheBundle;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::{debug, info};

/// Cache task for the block builder.
///
/// This tasks handles the ingestion of transactions and bundles into the cache.
/// It keeps a receiver for the block environment and cleans the cache when
/// the environment changes.
#[derive(Debug)]
pub struct CacheTask {
    /// The channel to receive the block environment.
    envs: watch::Receiver<Option<SimEnv>>,
    /// The channel to receive the transaction bundles.
    bundles: mpsc::UnboundedReceiver<TxCacheBundle>,
    /// The channel to receive the transactions.
    txns: mpsc::UnboundedReceiver<TxEnvelope>,
}

impl CacheTask {
    /// Create a new cache task with the given cache and channels.
    pub const fn new(
        env: watch::Receiver<Option<SimEnv>>,
        bundles: mpsc::UnboundedReceiver<TxCacheBundle>,
        txns: mpsc::UnboundedReceiver<TxEnvelope>,
    ) -> Self {
        Self { envs: env, bundles, txns }
    }

    async fn task_future(mut self, cache: SimCache) {
        loop {
            let mut basefee = 0;
            tokio::select! {
                biased;
                res = self.envs.changed() => {
                    if res.is_err() {
                        debug!("Cache task: env channel closed, exiting");
                        break;
                    }

                    if let Some(env) = self.envs.borrow_and_update().as_ref() {
                        let sim_env = env.rollup_env();

                        basefee = sim_env.basefee;
                        info!(basefee, block_env_number = sim_env.number.to::<u64>(), block_env_timestamp = sim_env.timestamp.to::<u64>(), "rollup block env changed, clearing cache");
                        cache.clean(
                            sim_env.number.to(), sim_env.timestamp.to()
                        );
                    }
                }
                Some(bundle) = self.bundles.recv() => {

                    let env_block = self.envs.borrow().as_ref().map(|e| e.rollup_env().number.to::<u64>()).unwrap_or_default();
                    let bundle_block = bundle.bundle.block_number();

                    // Don't insert bundles for past blocks
                    if env_block > bundle_block {
                        debug!(env.block = env_block, bundle.block = bundle_block, "skipping bundle insert");
                        continue;
                    }

                    let res = cache.add_bundle(bundle.bundle, basefee);
                    // Skip bundles that fail to be added to the cache
                    if let Err(e) = res {
                        debug!(?e, "Failed to add bundle to cache");
                        continue;
                    }
                }
                Some(txn) = self.txns.recv() => {
                    cache.add_tx(txn, basefee);
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
