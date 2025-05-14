mod task;
pub use task::CacheTask;

mod tx;
pub use tx::TxPoller;

mod bundle;
pub use bundle::BundlePoller;

use signet_sim::SimCache;
use tokio::task::JoinHandle;

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
