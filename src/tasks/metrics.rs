use crate::config::HostProvider;
use alloy::{
    primitives::TxHash,
    providers::{PendingTransactionBuilder, PendingTransactionError, Provider as _, WatchTxError},
};
use init4_bin_base::deps::{
    metrics::{counter, histogram},
    tracing::{Instrument, debug, error, info_span},
};
use std::time::{Duration, Instant};
use tokio::{sync::mpsc, task::JoinHandle};

/// Collects metrics on transactions sent by the Builder
#[derive(Debug, Clone)]
pub struct MetricsTask {
    /// Ethereum Provider
    pub host_provider: HostProvider,
}

impl MetricsTask {
    /// Given a transaction hash, record metrics on the result of the
    /// transaction mining
    pub fn log_tx(&self, tx_hash: TxHash) -> impl Future<Output = ()> + use<> {
        let provider = self.host_provider.clone();

        async move {
            // start timer when tx hash is received
            let start: Instant = Instant::now();

            let span = info_span!("metrics_submission", %tx_hash);

            // wait for the tx to mine, get its receipt
            let receipt = PendingTransactionBuilder::new(provider.root().clone(), tx_hash)
                .with_required_confirmations(1)
                .with_timeout(Some(Duration::from_secs(60)))
                .get_receipt()
                .instrument(span.clone())
                .await;

            // enter the span to log the result
            let _guard = span.entered();

            match receipt {
                Ok(receipt) => {
                    // record how long it took to mine the transaction
                    // potential improvement: use the block timestamp to calculate the time elapsed
                    histogram!("metrics.tx_mine_time").record(start.elapsed().as_millis() as f64);

                    // log whether the transaction reverted
                    if receipt.status() {
                        counter!("metrics.tx_reverted").increment(1);
                        debug!("tx reverted");
                    } else {
                        counter!("metrics.tx_succeeded").increment(1);
                        debug!("tx succeeded");
                    }
                }
                Err(PendingTransactionError::TxWatcher(WatchTxError::Timeout)) => {
                    // log that the transaction timed out
                    counter!("metrics.tx_not_mined").increment(1);
                    debug!("tx not mined");
                }
                Err(e) => {
                    counter!("metrics.rpc_error").increment(1);
                    error!(error = ?e, "rpc error");
                }
            }
        }
    }

    /// Spawns the task which collects metrics on pending transactions
    pub fn spawn(self) -> (mpsc::UnboundedSender<TxHash>, JoinHandle<()>) {
        let (sender, mut inbound) = mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            debug!("metrics task spawned");
            loop {
                let Some(tx_hash) = inbound.recv().await else {
                    debug!("upstream task gone");
                    break;
                };
                let fut = self.log_tx(tx_hash);
                tokio::spawn(fut);
            }
        });

        (sender, handle)
    }
}
