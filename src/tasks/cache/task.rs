use crate::tasks::{cache::tx::ReceivedTx, env::SimEnv};
use alloy::consensus::transaction::SignerRecoverable;
use init4_bin_base::deps::metrics::counter;
use signet_sim::SimCache;
use signet_tx_cache::types::CachedBundle;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::{debug, info};

/// Cache task for the block builder.
///
/// This task handles the ingestion of transactions and bundles into the cache.
/// It keeps a receiver for the block environment and cleans the cache when
/// the environment changes. Logs a per-block ingestion summary at INFO level
/// each time the block environment changes.
#[derive(Debug)]
pub struct CacheTask {
    /// The channel to receive the block environment.
    envs: watch::Receiver<Option<SimEnv>>,
    /// The channel to receive the transaction bundles.
    bundles: mpsc::UnboundedReceiver<CachedBundle>,
    /// The channel to receive the transactions.
    txns: mpsc::UnboundedReceiver<ReceivedTx>,
}

impl CacheTask {
    /// Create a new cache task with the given cache and channels.
    pub const fn new(
        env: watch::Receiver<Option<SimEnv>>,
        bundles: mpsc::UnboundedReceiver<CachedBundle>,
        txns: mpsc::UnboundedReceiver<ReceivedTx>,
    ) -> Self {
        Self { envs: env, bundles, txns }
    }

    async fn task_future(mut self, cache: SimCache) {
        let mut summary = IngestionSummary::default();

        loop {
            let mut basefee = 0;
            tokio::select! {
                biased;
                res = self.envs.changed() => {
                    if res.is_err() {
                        summary.log_and_reset();
                        debug!("Cache task: env channel closed, exiting");
                        break;
                    }

                    if let Some(env) = self.envs.borrow_and_update().as_ref() {
                        let _guard = env.span().enter();
                        let sim_env = env.rollup_env();

                        summary.log_and_reset();

                        basefee = sim_env.basefee;
                        info!(
                            basefee,
                            block_env_number = sim_env.number.to::<u64>(),
                            block_env_timestamp = sim_env.timestamp.to::<u64>(),
                            "rollup block env changed, clearing cache"
                        );
                        cache.clean(
                            sim_env.number.to(), sim_env.timestamp.to()
                        );
                        counter!("signet.builder.cache.cache_cleans").increment(1);
                    }
                }
                Some(bundle) = self.bundles.recv() => {
                    summary.bundles_received += 1;

                    let env_block = self.envs.borrow()
                        .as_ref()
                        .map(|e| e.rollup_env().number.to::<u64>())
                        .unwrap_or_default();
                    let bundle_block = bundle.bundle.block_number();

                    // Don't insert bundles for past blocks
                    if env_block > bundle_block {
                        debug!(
                            env.block = env_block,
                            bundle.block = bundle_block,
                            %bundle.id,
                            "skipping bundle insert"
                        );
                        counter!("signet.builder.cache.bundles_skipped").increment(1);
                        summary.bundles_skipped_stale += 1;
                        continue;
                    }

                    let res = cache.add_bundle(bundle.bundle, basefee);
                    // Skip bundles that fail to be added to the cache
                    if let Err(e) = res {
                        counter!("signet.builder.cache.bundle_add_errors").increment(1);
                        debug!(?e, "Failed to add bundle to cache");
                        continue;
                    }
                    counter!("signet.builder.cache.bundles_ingested").increment(1);
                    summary.bundles_accepted += 1;
                }
                Some(received_txn) = self.txns.recv() => {
                    summary.txs_received += 1;
                    let ReceivedTx::Tx(txn) = received_txn else {
                        summary.txs_skipped_stale_nonce += 1;
                        continue;
                    };

                    match txn.try_into_recovered() {
                        Ok(recovered_tx) => {
                            cache.add_tx(recovered_tx, basefee);
                            counter!("signet.builder.cache.txs_ingested").increment(1);
                            summary.txs_accepted += 1;
                        }
                        Err(_) => {
                            counter!("signet.builder.cache.tx_recover_failures").increment(1);
                            debug!("Failed to recover transaction signature");
                        }
                    }
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

/// Per-block counters for the cache ingestion summary log.
#[derive(Debug, Default)]
struct IngestionSummary {
    /// Set after the first call to `log_and_reset`, so we skip the spurious summary before any
    /// real block has been processed.
    has_logged: bool,
    bundles_received: u64,
    bundles_accepted: u64,
    bundles_skipped_stale: u64,
    txs_received: u64,
    txs_accepted: u64,
    txs_skipped_stale_nonce: u64,
}

impl IngestionSummary {
    /// Logs a per-block ingestion summary at INFO level and resets the counters.
    ///
    /// The first call is a no-op (there is no previous block to summarise).
    fn log_and_reset(&mut self) {
        if !self.has_logged {
            self.has_logged = true;
            return;
        }

        if self.bundles_received == 0 && self.txs_received == 0 {
            debug_assert_eq!(self.bundles_accepted, 0);
            debug_assert_eq!(self.bundles_skipped_stale, 0);
            debug_assert_eq!(self.txs_accepted, 0);
            debug_assert_eq!(self.txs_skipped_stale_nonce, 0);
            info!("cache ingestion summary: no bundles or transactions received");
        } else {
            let bundles_handling_error = self
                .bundles_received
                .saturating_sub(self.bundles_accepted)
                .saturating_sub(self.bundles_skipped_stale);
            let txs_handling_error = self
                .txs_received
                .saturating_sub(self.txs_accepted)
                .saturating_sub(self.txs_skipped_stale_nonce);
            info!(
                bundles_received = self.bundles_received,
                bundles_accepted = self.bundles_accepted,
                bundles_skipped_stale = self.bundles_skipped_stale,
                bundles_handling_error,
                txs_received = self.txs_received,
                txs_accepted = self.txs_accepted,
                txs_skipped_stale_nonce = self.txs_skipped_stale_nonce,
                txs_handling_error,
                "cache ingestion summary"
            );
        }

        *self = IngestionSummary { has_logged: true, ..IngestionSummary::default() };
    }
}
