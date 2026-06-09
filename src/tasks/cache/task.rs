use crate::tasks::{cache::tx::ReceivedTx, env::SimEnv};
use alloy::consensus::transaction::SignerRecoverable;
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

    /// Returns the rollup block number and basefee of the current sim env, or
    /// `(0, 0)` if no env has been published yet.
    ///
    /// Read at ingest time rather than cached across `select!` iterations: the
    /// loop re-enters `select!` for every channel event, so a value assigned in
    /// the `envs.changed()` arm is not visible when a bundle or transaction is
    /// later received in a different arm. Reading the live env here ensures
    /// items are ranked against the correct basefee.
    fn current_block_and_basefee(&self) -> (u64, u64) {
        self.envs
            .borrow()
            .as_ref()
            .map(|env| {
                let rollup = env.rollup_env();
                (rollup.number.to::<u64>(), rollup.basefee)
            })
            .unwrap_or_default()
    }

    async fn task_future(mut self, cache: SimCache) {
        let mut summary = IngestionSummary::default();

        loop {
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

                        let basefee = sim_env.basefee;
                        info!(
                            basefee,
                            block_env_number = sim_env.number.to::<u64>(),
                            block_env_timestamp = sim_env.timestamp.to::<u64>(),
                            "rollup block env changed, clearing cache"
                        );
                        cache.clean(
                            sim_env.number.to(), sim_env.timestamp.to()
                        );
                        crate::metrics::inc_cache_cleans();
                    }
                }
                Some(bundle) = self.bundles.recv() => {
                    summary.bundles_received += 1;

                    // Read the block number and basefee from the live env at
                    // ingest time (see `current_block_and_basefee`).
                    let (env_block, basefee) = self.current_block_and_basefee();
                    let bundle_block = bundle.bundle.block_number();

                    // Don't insert bundles for past blocks
                    if env_block > bundle_block {
                        debug!(
                            env.block = env_block,
                            bundle.block = bundle_block,
                            %bundle.id,
                            "skipping bundle insert"
                        );
                        crate::metrics::inc_bundles_skipped();
                        summary.bundles_skipped_stale += 1;
                        continue;
                    }

                    let res = cache.add_bundle(bundle.bundle, basefee);
                    // Skip bundles that fail to be added to the cache
                    if let Err(e) = res {
                        crate::metrics::inc_bundle_add_errors();
                        debug!(?e, "Failed to add bundle to cache");
                        continue;
                    }
                    crate::metrics::inc_bundles_ingested();
                    summary.bundles_accepted += 1;
                }
                Some(received_txn) = self.txns.recv() => {
                    summary.txs_received += 1;
                    let ReceivedTx::Tx(txn) = received_txn else {
                        summary.txs_skipped_stale_nonce += 1;
                        continue;
                    };

                    let (_, basefee) = self.current_block_and_basefee();
                    match txn.try_into_recovered() {
                        Ok(recovered_tx) => {
                            cache.add_tx(recovered_tx, basefee);
                            crate::metrics::inc_txs_ingested();
                            summary.txs_accepted += 1;
                        }
                        Err(_) => {
                            crate::metrics::inc_tx_recover_failures();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::env::Environment;
    use crate::test_utils::{create_transfer_tx, scenarios_test_block_env as test_block_env};
    use alloy::consensus::TxEnvelope;
    use alloy::primitives::U256;
    use alloy::signers::local::PrivateKeySigner;
    use signet_sim::SimItem;
    use std::time::Duration;

    // Regression for the `basefee = 0` ingest bug: `basefee` was re-initialised
    // to 0 at the top of every `select!` iteration and assigned only in the
    // `envs.changed()` arm, so `add_tx`/`add_bundle` (which run in other arms)
    // always ranked items with basefee 0. The ingest arms must read the basefee
    // from the live env instead.
    #[tokio::test]
    async fn cache_task_ranks_with_env_basefee() {
        const BASEFEE: u64 = 60_000_000_000; // 60 gwei, below the tx's 100 gwei max fee
        const PRIORITY: u128 = 50_000_000_000; // 50 gwei tip

        // Env whose rollup block carries BASEFEE.
        let block_env = test_block_env(100, BASEFEE, 1_000, 30_000_000);
        let sim_env = SimEnv {
            rollup: Environment::new(block_env, Default::default()),
            host: Environment::for_testing(),
            span: tracing::Span::none(),
        };

        let (_env_tx, env_rx) = watch::channel(Some(sim_env));
        let (_bundle_tx, bundle_rx) = mpsc::unbounded_channel();
        let (txn_tx, txn_rx) = mpsc::unbounded_channel();

        let (cache, _jh) = CacheTask::new(env_rx, bundle_rx, txn_rx).spawn();

        // A tx whose effective tip depends on the basefee: with max_fee 100 gwei
        // and tip 50 gwei, the tip is min(50, 100-60)=40 gwei at BASEFEE vs
        // 50 gwei at basefee 0, so the two ranks differ.
        let signer = PrivateKeySigner::random();
        let recovered =
            create_transfer_tx(&signer, signer.address(), U256::from(1u64), 0, 1, PRIORITY)
                .unwrap();
        let envelope: TxEnvelope = recovered.inner().clone();

        let item = SimItem::from(recovered);
        let expected = item.calculate_total_fee(BASEFEE);
        let with_zero = item.calculate_total_fee(0);
        assert_ne!(
            expected, with_zero,
            "basefee must affect the rank for this test to be meaningful"
        );

        txn_tx.send(ReceivedTx::Tx(envelope)).unwrap();

        // Wait for the task to ingest the tx.
        let mut rank = None;
        for _ in 0..200 {
            if let Some((r, _)) = cache.read_best(1).into_iter().next() {
                rank = Some(r);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(
            rank.expect("tx was never ingested into the cache"),
            expected,
            "tx must be ranked with the live env basefee, not 0",
        );
    }
}
