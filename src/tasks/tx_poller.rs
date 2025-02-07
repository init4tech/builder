use alloy::consensus::TxEnvelope;
use eyre::Error;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::from_slice;
use tokio::{sync::mpsc, task::JoinHandle};

pub use crate::config::BuilderConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxPoolResponse {
    transactions: Vec<TxEnvelope>,
}

/// Implements a poller for the block builder to pull transactions from the transaction pool.
pub struct TxPoller {
    // config for the builder
    pub config: BuilderConfig,
    // Reqwest client for fetching transactions from the tx-pool
    pub client: Client,
}

/// TxPoller implements a poller task that fetches unique transactions from the transaction pool.
impl TxPoller {
    /// Returns a new TxPoller with the given config.
    pub fn new(config: &BuilderConfig) -> Self {
        Self { config: config.clone(), client: Client::new() }
    }

    /// Polls the tx-pool for unique transactions and evicts expired transactions.
    /// unique transactions that haven't been seen before are sent into the builder pipeline.
    pub async fn check_tx_cache(&mut self) -> Result<Vec<TxEnvelope>, Error> {
        let url: Url = Url::parse(&self.config.tx_pool_url)?.join("transactions")?;
        let result = self.client.get(url).send().await?;
        let response: TxPoolResponse = from_slice(result.text().await?.as_bytes())?;
        Ok(response.transactions)
    }

    /// Spawns a task that trawls the cache for transactions and sends along anything it finds
    pub fn spawn(mut self) -> (mpsc::UnboundedReceiver<TxEnvelope>, JoinHandle<()>) {
        let (outbound, inbound) = mpsc::unbounded_channel();
        let jh = tokio::spawn(async move {
            loop {
                if let Ok(transactions) = self.check_tx_cache().await {
                    tracing::debug!(count = ?transactions.len(), "found transactions");
                    for tx in transactions.iter() {
                        if let Err(err) = outbound.send(tx.clone()) {
                            tracing::error!(err = ?err, "failed to send transaction outbound");
                        }
                    }
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        });
        (inbound, jh)
    }
}
