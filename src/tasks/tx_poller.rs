use std::time::Duration;
use std::{collections::HashMap, time};

use alloy::consensus::TxEnvelope;
use alloy_primitives::TxHash;

use eyre::Error;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::from_slice;

pub use crate::config::BuilderConfig;

use metrics::counter;

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
    //  Maintain a set of transaction hashes to their expiration times
    pub seen_txns: HashMap<TxHash, time::Instant>,
}

/// TxPoller implements a poller that fetches unique transactions from the transaction pool.
impl TxPoller {
    /// returns a new TxPoller with the given config.
    pub fn new(config: &BuilderConfig) -> Self {
        Self { config: config.clone(), client: Client::new(), seen_txns: HashMap::new() }
    }

    /// polls the tx-pool for unique transactions and evicts expired transactions.
    /// unique transactions that haven't been seen before are sent into the builder pipeline.
    pub async fn check_tx_cache(&mut self) -> Result<Vec<TxEnvelope>, Error> {
        let mut unique: Vec<TxEnvelope> = Vec::new();

        let url: Url = Url::parse(&self.config.tx_pool_url)?.join("transactions")?;
        let result = self.client.get(url).send().await?;
        let response: TxPoolResponse = from_slice(result.text().await?.as_bytes())?;

        response.transactions.iter().for_each(|entry| {
            self.check_seen_txs(entry.clone(), &mut unique);
        });

        Ok(unique)
    }

    /// checks if the transaction has been seen before and if not, adds it to the unique transactions list.
    fn check_seen_txs(&mut self, tx: TxEnvelope, unique: &mut Vec<TxEnvelope>) {
        self.seen_txns.entry(*tx.tx_hash()).or_insert_with(|| {
            // add to unique transactions
            unique.push(tx.clone());
            counter!("builder.unique_txs").increment(1);
            // expiry is now + cache_duration
            time::Instant::now() + Duration::from_secs(self.config.tx_pool_cache_duration)
        });
    }

    /// removes entries from seen_txns that have lived past expiry
    pub fn evict(&mut self) {
        let expired_keys: Vec<TxHash> = self
            .seen_txns
            .iter()
            .filter_map(
                |(key, &expiration)| {
                    if !expiration.elapsed().is_zero() {
                        Some(*key)
                    } else {
                        None
                    }
                },
            )
            .collect();

        for key in expired_keys {
            self.seen_txns.remove(&key);
            counter!("builder.evicted_txs").increment(1);
        }
    }
}
