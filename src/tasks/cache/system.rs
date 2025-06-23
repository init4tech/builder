use signet_sim::SimCache;
use tokio::{sync::watch, task::JoinHandle};

use crate::{
    config::BuilderConfig,
    tasks::{
        cache::{BundlePoller, CacheTask, TxPoller},
        env::SimEnv,
    },
};

/// Cache tasks for the block builder.
#[derive(Debug)]
pub struct CacheSystem {
    /// The builder config.
    pub config: BuilderConfig,
}

impl CacheSystem {
    /// Create a new [`CacheSystem`] with the given components.
    pub const fn new(config: BuilderConfig) -> Self {
        Self { config }
    }

    /// Spawn a new [`CacheSystem`], which spawns the
    /// [`CacheTask`], [`TxPoller`], and [`BundlePoller`] internally.
    pub fn spawn(
        &self,
        block_env: watch::Receiver<Option<SimEnv>>,
    ) -> (SimCache, JoinHandle<()>, JoinHandle<()>, JoinHandle<()>) {
        // Tx Poller pulls transactions from the cache
        let tx_poller = TxPoller::new(&self.config);
        let (tx_receiver, tx_poller) = tx_poller.spawn();

        // Bundle Poller pulls bundles from the cache
        let bundle_poller = BundlePoller::new(&self.config, self.config.oauth_token());
        let (bundle_receiver, bundle_poller) = bundle_poller.spawn();

        // Set up the cache task
        let cache_task = CacheTask::new(block_env.clone(), bundle_receiver, tx_receiver);
        let (sim_cache, cache_task) = cache_task.spawn();

        (sim_cache, tx_poller, bundle_poller, cache_task)
    }
}
