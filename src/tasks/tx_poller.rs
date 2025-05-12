//! Transaction service responsible for fetching and sending trasnsactions to the simulator.
use crate::config::BuilderConfig;
use alloy::consensus::TxEnvelope;
use eyre::Error;
use init4_bin_base::deps::tracing::{Instrument, debug, debug_span, trace};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{sync::mpsc, task::JoinHandle, time};

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

/// [`TxPoller`] implements a poller task that fetches transactions from the transaction pool
/// and sends them into the provided channel sender.
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

    /// Returns the poll duration as a [`Duration`].
    const fn poll_duration(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    /// Polls the transaction cache for transactions.
    pub async fn check_tx_cache(&mut self) -> Result<Vec<TxEnvelope>, Error> {
        let url: Url = Url::parse(&self.config.tx_pool_url)?.join("transactions")?;
        self.client
            .get(url)
            .send()
            .await?
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
                    if outbound.send(tx).is_err() {
                        // If there are no receivers, we can shut down
                        trace!("No receivers left, shutting down");
                        break;
                    }
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
