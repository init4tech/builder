use signet_sim::SimCache;
use tokio::{sync::watch, task::JoinHandle};

use crate::{
    config::BuilderConfig,
    tasks::{
        cache::{BundlePoller, CacheTask, TxPoller},
        env::SimEnv,
    },
};

/// The block builder's cache system.
#[derive(Debug)]
pub struct CacheTasks {
    /// The builder config.
    pub config: BuilderConfig,
}

impl CacheTasks {
    /// Create a new [`CacheSystem`] with the given components.
    pub const fn new(config: BuilderConfig) -> Self {
        Self { config }
    }

    /// Spawn a new [`CacheSystem`], which starts the
    /// [`CacheTask`], [`TxPoller`], and [`BundlePoller`] internally and yields their [`JoinHandle`]s.
    pub fn spawn(&self, block_env: watch::Receiver<Option<SimEnv>>) -> CacheSystem {
        // Tx Poller pulls transactions from the cache
        let tx_poller = TxPoller::new(&self.config);
        let (tx_receiver, tx_poller) = tx_poller.spawn();

        // Bundle Poller pulls bundles from the cache
        let bundle_poller = BundlePoller::new(&self.config, self.config.oauth_token());
        let (bundle_receiver, bundle_poller) = bundle_poller.spawn();

        // Set up the cache task
        let cache_task = CacheTask::new(block_env.clone(), bundle_receiver, tx_receiver);
        let (sim_cache, cache_task) = cache_task.spawn();

        CacheSystem::new(sim_cache, tx_poller, bundle_poller, cache_task)
    }
}

/// The tasks that the cache system spawns.
////// This struct contains the cache and the task handles for the
/// [`CacheTask`], [`TxPoller`], and [`BundlePoller`].
#[derive(Debug)]
pub struct CacheSystem {
    /// The cache for the block builder.
    pub sim_cache: SimCache,
    /// The task handle for the transaction poller.
    pub tx_poller: JoinHandle<()>,
    /// The task handle for the bundle poller.
    pub bundle_poller: JoinHandle<()>,
    /// The task handle for the cache task.
    pub cache_task: JoinHandle<()>,
}

impl CacheSystem {
    /// Create a new [`CacheTasks`] instance.
    pub const fn new(
        sim_cache: SimCache,
        tx_poller: JoinHandle<()>,
        bundle_poller: JoinHandle<()>,
        cache_task: JoinHandle<()>,
    ) -> Self {
        Self { sim_cache, tx_poller, bundle_poller, cache_task }
    }
}
