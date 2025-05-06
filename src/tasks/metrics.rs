use crate::config::HostProvider;
use alloy::{primitives::TxHash, providers::Provider as _};
use init4_bin_base::deps::metrics::{counter, histogram};
use std::time::Instant;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, error};

/// Collects metrics on transactions sent by the Builder
#[derive(Debug, Clone)]
pub struct MetricsTask {
    /// Ethereum Provider
    pub host_provider: HostProvider,
}

impl MetricsTask {
    /// Given a transaction hash, record metrics on the result of the transaction mining
    pub async fn log_tx(&self, pending_tx_hash: TxHash) {
        // start timer when tx hash is received
        let start: Instant = Instant::now();

        // wait for the tx to mine, get its receipt
        let receipt_result =
            self.host_provider.clone().get_transaction_receipt(pending_tx_hash).await;

        match receipt_result {
            Ok(maybe_receipt) => {
                match maybe_receipt {
                    Some(receipt) => {
                        // record how long it took to mine the transaction
                        // potential improvement: use the block timestamp to calculate the time elapsed
                        histogram!("metrics.tx_mine_time")
                            .record(start.elapsed().as_millis() as f64);

                        // log whether the transaction reverted
                        if receipt.status() {
                            counter!("metrics.tx_reverted").increment(1);
                            debug!(tx_hash = %pending_tx_hash, "tx reverted");
                        } else {
                            counter!("metrics.tx_succeeded").increment(1);
                            debug!(tx_hash = %pending_tx_hash, "tx succeeded");
                        }
                    }
                    None => {
                        counter!("metrics.no_receipt").increment(1);
                        error!("no receipt found for tx hash");
                    }
                }
            }
            Err(e) => {
                counter!("metrics.rpc_error").increment(1);
                error!(error = ?e, "rpc error");
            }
        }
    }

    /// Spawns the task which collects metrics on pending transactions
    pub fn spawn(self) -> (mpsc::UnboundedSender<TxHash>, JoinHandle<()>) {
        let (sender, mut inbound) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            debug!("metrics task spawned");
            loop {
                if let Some(pending_tx_hash) = inbound.recv().await {
                    let this = self.clone();
                    tokio::spawn(async move {
                        debug!("received tx hash");
                        let that = this.clone();
                        that.log_tx(pending_tx_hash).await;
                        debug!("logged tx metrics");
                    });
                } else {
                    tracing::debug!("upstream task gone");
                    break;
                }
            }
        });

        (sender, handle)
    }
}
