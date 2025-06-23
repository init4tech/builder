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
    /// The cache task.
    pub cache_task: JoinHandle<()>,

    /// The transaction poller task.
    pub tx_poller: JoinHandle<()>,

    /// The bundle poller task.
    pub bundle_poller: JoinHandle<()>,

    /// The sim cache.
    pub sim_cache: SimCache,
}

impl CacheSystem {
    /// Spawn a new [`CacheSystem`]. This contains the
    /// joinhandles for [`TxPoller`] and [`BundlePoller`] and [`CacheTask`], as
    /// well as the [`SimCache`] and the block env watcher.
    ///
    /// [`SimCache`]: signet_sim::SimCache
    pub fn spawn(
        config: &BuilderConfig,
        block_env: watch::Receiver<Option<SimEnv>>,
    ) -> CacheSystem {
        // Tx Poller pulls transactions from the cache
        let tx_poller = TxPoller::new(config);
        let (tx_receiver, tx_poller) = tx_poller.spawn();

        // Bundle Poller pulls bundles from the cache
        let bundle_poller = BundlePoller::new(config, config.oauth_token());
        let (bundle_receiver, bundle_poller) = bundle_poller.spawn();

        // Set up the cache task
        let cache_task = CacheTask::new(block_env.clone(), bundle_receiver, tx_receiver);
        let (sim_cache, cache_task) = cache_task.spawn();

        CacheSystem { cache_task, tx_poller, bundle_poller, sim_cache }
    }
}
