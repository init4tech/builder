use alloy::consensus::TxEnvelope;
use eyre::Error;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::from_slice;

pub use crate::config::BuilderConfig;

/// Response from the tx-pool endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxPoolResponse {
    transactions: Vec<TxEnvelope>,
}

/// Implements a poller for the block builder to pull transactions from the transaction pool.
#[derive(Debug)]
pub struct TxPoller {
    /// config for the builder
    pub config: BuilderConfig,
    /// Reqwest client for fetching transactions from the tx-pool
    pub client: Client,
}

/// TxPoller implements a poller that fetches unique transactions from the transaction pool.
impl TxPoller {
    /// returns a new TxPoller with the given config.
    pub fn new(config: &BuilderConfig) -> Self {
        Self { config: config.clone(), client: Client::new() }
    }

    /// polls the tx-pool for unique transactions and evicts expired transactions.
    /// unique transactions that haven't been seen before are sent into the builder pipeline.
    pub async fn check_tx_cache(&mut self) -> Result<Vec<TxEnvelope>, Error> {
        let url: Url = Url::parse(&self.config.tx_pool_url)?.join("transactions")?;
        let result = self.client.get(url).send().await?;
        let response: TxPoolResponse = from_slice(result.text().await?.as_bytes())?;
        Ok(response.transactions)
    }
}
