//! Transaction service responsible for fetching and sending trasnsactions to the simulator.
use crate::config::BuilderConfig;
use alloy::consensus::TxEnvelope;
use eyre::Error;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use tokio::{sync::mpsc, task::JoinHandle};

/// Models a response from the transaction pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxPoolResponse {
    /// Holds the transactions property as a list on the response.
    transactions: Vec<TxEnvelope>,
}

/// Implements a poller for the block builder to pull transactions from the transaction pool.
#[derive(Debug, Clone)]
pub struct TxPoller {
    /// Config values from the Builder.
    pub config: BuilderConfig,
    /// Reqwest Client for fetching transactions from the cache.
    pub client: Client,
    /// Defines the interval at which the service should poll the cache.
    pub poll_interval_ms: u64,
}

/// [`TxPoller`] implements a poller task that fetches unique transactions from the transaction pool.
impl TxPoller {
    /// Returns a new [`TxPoller`] with the given config.
    /// * Defaults to 1000ms poll interval (1s).
    pub fn new(config: &BuilderConfig) -> Self {
        Self { config: config.clone(), client: Client::new(), poll_interval_ms: 1000 }
    }

    /// Returns a new [`TxPoller`] with the given config and cache polling interval in milliseconds.
    pub fn new_with_poll_interval_ms(config: &BuilderConfig, poll_interval_ms: u64) -> Self {
        Self { config: config.clone(), client: Client::new(), poll_interval_ms }
    }

    /// Polls the transaction cache for transactions.
    pub async fn check_tx_cache(&mut self) -> Result<Vec<TxEnvelope>, Error> {
        let url: Url = Url::parse(&self.config.tx_pool_url)?.join("transactions")?;
        let result = self.client.get(url).send().await?;
        let response: TxPoolResponse = from_slice(result.text().await?.as_bytes())?;
        Ok(response.transactions)
    }

    /// Spawns a task that continuously polls the cache for transactions and sends any it finds to its sender.
    pub fn spawn(mut self) -> (mpsc::UnboundedReceiver<TxEnvelope>, JoinHandle<()>) {
        let (outbound, inbound) = mpsc::unbounded_channel();
        let jh = tokio::spawn(async move {
            loop {
                if let Ok(transactions) = self.check_tx_cache().await {
                    tracing::debug!(count = ?transactions.len(), "found transactions");
                    for tx in transactions.into_iter() {
                        if let Err(err) = outbound.send(tx) {
                            tracing::error!(err = ?err, "failed to send transaction - channel is dropped.");
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(self.poll_interval_ms)).await;
            }
        });
        (inbound, jh)
    }
}
