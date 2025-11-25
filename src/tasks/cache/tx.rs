//! Transaction service responsible for fetching and sending trasnsactions to the simulator.
use crate::config::BuilderConfig;
use alloy::{
    consensus::{Transaction, TxEnvelope, transaction::SignerRecoverable},
    providers::Provider,
};
use eyre::Error;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{sync::mpsc, task::JoinHandle, time};
use tracing::{Instrument, debug, debug_span, info_span, trace};

/// Poll interval for the transaction poller in milliseconds.
const POLL_INTERVAL_MS: u64 = 1000;

/// Models a response from the transaction pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TxPoolResponse {
    /// Holds the transactions property as a list on the response.
    transactions: Vec<TxEnvelope>,
}

/// Implements a poller for the block builder to pull transactions from the
/// transaction pool.
#[derive(Debug, Clone)]
pub struct TxPoller {
    /// Config values from the Builder.
    config: &'static BuilderConfig,
    /// Reqwest Client for fetching transactions from the cache.
    client: Client,
    /// Defines the interval at which the service should poll the cache.
    poll_interval_ms: u64,
}

impl Default for TxPoller {
    fn default() -> Self {
        Self::new()
    }
}

/// [`TxPoller`] implements a poller task that fetches transactions from the transaction pool
/// and sends them into the provided channel sender.
impl TxPoller {
    /// Returns a new [`TxPoller`] with the given config.
    /// * Defaults to 1000ms poll interval (1s).
    pub fn new() -> Self {
        Self::new_with_poll_interval_ms(POLL_INTERVAL_MS)
    }

    /// Returns a new [`TxPoller`] with the given config and cache polling interval in milliseconds.
    pub fn new_with_poll_interval_ms(poll_interval_ms: u64) -> Self {
        let config = crate::config();
        Self { config, client: Client::new(), poll_interval_ms }
    }

    /// Returns the poll duration as a [`Duration`].
    const fn poll_duration(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    // Spawn a tokio task to check the nonce of a transaction before sending
    // it to the cachetask via the outbound channel.
    fn spawn_check_nonce(&self, tx: TxEnvelope, outbound: mpsc::UnboundedSender<TxEnvelope>) {
        tokio::spawn(async move {
            let span = info_span!("check_nonce", tx_id = %tx.tx_hash());

            let Ok(ru_provider) =
                crate::config().connect_ru_provider().instrument(span.clone()).await
            else {
                span_warn!(span, "Failed to connect to RU provider, stopping noncecheck task.");
                return;
            };

            let Ok(sender) = tx.recover_signer() else {
                span_warn!(span, "Failed to recover sender from transaction");
                return;
            };

            let Ok(tx_count) = ru_provider
                .get_transaction_count(sender)
                .into_future()
                .instrument(span.clone())
                .await
            else {
                span_warn!(span, %sender, "Failed to fetch nonce for sender");
                return;
            };

            if tx.nonce() < tx_count {
                span_debug!(span, %sender, tx_nonce = %tx.nonce(), ru_nonce = %tx_count, "Dropping transaction with stale nonce");
                return;
            }

            if outbound.send(tx).is_err() {
                span_warn!(span, "Outbound channel closed, stopping NonceChecker task.");
            }
        });
    }

    /// Polls the transaction cache for transactions.
    pub async fn check_tx_cache(&mut self) -> Result<Vec<TxEnvelope>, Error> {
        let url: Url = self.config.tx_pool_url.join("transactions")?;
        self.client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .map(|resp: TxPoolResponse| resp.transactions)
            .map_err(Into::into)
    }

    async fn task_future(mut self, outbound: mpsc::UnboundedSender<TxEnvelope>) {
        loop {
            let span = debug_span!("TxPoller::loop", url = %self.config.tx_pool_url);

            // Enter the span for the next check.
            let _guard = span.enter();

            // Check this here to avoid making the web request if we know
            // we don't need the results.
            if outbound.is_closed() {
                trace!("No receivers left, shutting down");
                break;
            }
            // exit the span after the check.
            drop(_guard);

            if let Ok(transactions) =
                self.check_tx_cache().instrument(span.clone()).await.inspect_err(|err| {
                    debug!(%err, "Error fetching transactions");
                })
            {
                let _guard = span.entered();
                debug!(count = ?transactions.len(), "found transactions");
                for tx in transactions.into_iter() {
                    self.spawn_check_nonce(tx, outbound.clone());
                }
            }

            time::sleep(self.poll_duration()).await;
        }
    }

    /// Spawns a task that continuously polls the cache for transactions and sends any it finds to its sender.
    pub fn spawn(self) -> (mpsc::UnboundedReceiver<TxEnvelope>, JoinHandle<()>) {
        let (outbound, inbound) = mpsc::unbounded_channel();
        let jh = tokio::spawn(self.task_future(outbound));
        (inbound, jh)
    }
}
