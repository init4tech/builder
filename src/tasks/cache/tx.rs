//! Transaction service responsible for fetching and sending transactions to the simulator.
use crate::{config::BuilderConfig, tasks::env::SimEnv};
use alloy::{
    consensus::{Transaction, TxEnvelope, transaction::SignerRecoverable},
    providers::Provider,
};
use futures_util::{Stream, StreamExt, TryFutureExt, TryStreamExt};
use init4_bin_base::deps::metrics::{counter, histogram};
use signet_tx_cache::{TxCache, TxCacheError};
use std::{pin::Pin, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};
use tracing::{Instrument, debug, debug_span, trace, trace_span, warn};

type SseStream = Pin<Box<dyn Stream<Item = Result<TxEnvelope, TxCacheError>> + Send>>;

/// Implements a poller for the block builder to pull transactions from the
/// transaction pool.
#[derive(Debug)]
pub struct TxPoller {
    /// Config values from the Builder.
    config: &'static BuilderConfig,
    /// Client for the tx cache.
    tx_cache: TxCache,
    /// Receiver for block environment updates, used to trigger refetches.
    envs: watch::Receiver<Option<SimEnv>>,
}

/// [`TxPoller`] fetches transactions from the transaction pool on startup
/// and on each block environment change, and subscribes to an SSE stream
/// for real-time delivery of new transactions in between.
impl TxPoller {
    const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
    const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);

    /// Returns a new [`TxPoller`] with the given block environment receiver.
    pub fn new(envs: watch::Receiver<Option<SimEnv>>) -> Self {
        let config = crate::config();
        let tx_cache = TxCache::new(config.tx_pool_url.clone());
        Self { config, tx_cache, envs }
    }

    /// Spawn a tokio task to check the nonce of a transaction before sending
    /// it to the cachetask via the outbound channel.
    fn spawn_check_nonce(&self, tx: TxEnvelope, outbound: mpsc::UnboundedSender<ReceivedTx>) {
        tokio::spawn(async move {
            let span = debug_span!("check_nonce", tx_id = %tx.tx_hash());

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
                crate::metrics::inc_tx_nonce_stale();
                if outbound.send(ReceivedTx::StaleNonce).is_err() {
                    span_warn!(span, "Outbound channel closed, stopping NonceChecker task.");
                }
                span_debug!(span, %sender, tx_nonce = %tx.nonce(), ru_nonce = %tx_count, "Dropping transaction with stale nonce");
                return;
            }

            if outbound.send(ReceivedTx::Tx(tx)).is_err() {
                span_warn!(span, "Outbound channel closed, stopping NonceChecker task.");
            }
        });
    }

    /// Fetches all transactions from the cache, forwarding each to nonce
    /// checking before it reaches the cache task.
    async fn full_fetch(&self, outbound: &mpsc::UnboundedSender<ReceivedTx>) {
        let span = trace_span!("TxPoller::full_fetch", url = %self.config.tx_pool_url);

        counter!("signet.builder.cache.tx_poll_count").increment(1);
        if let Ok(transactions) = self
            .tx_cache
            .stream_transactions()
            .try_collect::<Vec<_>>()
            .inspect_err(|error| {
                counter!("signet.builder.cache.tx_poll_errors").increment(1);
                debug!(%error, "Error fetching transactions");
            })
            .instrument(span.clone())
            .await
        {
            let _guard = span.entered();
            histogram!("signet.builder.cache.txs_fetched").record(transactions.len() as f64);
            trace!(count = transactions.len(), "found transactions");
            for tx in transactions {
                self.spawn_check_nonce(tx, outbound.clone());
            }
        }
    }

    /// Opens an SSE subscription to the transaction feed. Returns an empty
    /// stream on connection failure so the caller can handle reconnection
    /// uniformly.
    async fn subscribe(&self) -> SseStream {
        match self.tx_cache.subscribe_transactions().await {
            Ok(stream) => {
                debug!(url = %self.config.tx_pool_url, "SSE transaction subscription established");
                Box::pin(stream)
            }
            Err(error) => {
                warn!(%error, "Failed to open SSE transaction subscription");
                Box::pin(futures_util::stream::empty())
            }
        }
    }

    /// Reconnects the SSE stream with backoff. Performs a full refetch to
    /// cover any items missed while disconnected.
    async fn reconnect(
        &mut self,
        outbound: &mpsc::UnboundedSender<ReceivedTx>,
        backoff: &mut Duration,
    ) -> SseStream {
        tokio::select! {
            _ = time::sleep(*backoff) => {}
            // Break the sleep early on block env change or channel close —
            // full_fetch below serves the same purpose the env arm would have.
            _ = self.envs.changed() => {}
        }
        *backoff = (*backoff * 2).min(Self::MAX_RECONNECT_BACKOFF);
        self.full_fetch(outbound).await;
        self.subscribe().await
    }

    async fn task_future(mut self, outbound: mpsc::UnboundedSender<ReceivedTx>) {
        // Initial full fetch of all transactions currently in the cache.
        self.full_fetch(&outbound).await;

        // Open the SSE stream for real-time delivery of new transactions.
        let mut sse_stream = self.subscribe().await;
        let mut backoff = Self::INITIAL_RECONNECT_BACKOFF;

        loop {
            tokio::select! {
                item = sse_stream.next() => {
                    match item {
                        Some(Ok(tx)) => {
                            backoff = Self::INITIAL_RECONNECT_BACKOFF;
                            if outbound.is_closed() {
                                trace!("No receivers left, shutting down");
                                break;
                            }
                            self.spawn_check_nonce(tx, outbound.clone());
                        }
                        Some(Err(error)) => {
                            warn!(%error, "SSE transaction stream error, reconnecting");
                            sse_stream = self.reconnect(&outbound, &mut backoff).await;
                        }
                        None => {
                            warn!("SSE transaction stream ended, reconnecting");
                            sse_stream = self.reconnect(&outbound, &mut backoff).await;
                        }
                    }
                }
                res = self.envs.changed() => {
                    if res.is_err() {
                        debug!("Block env channel closed, shutting down");
                        break;
                    }
                    trace!("Block env changed, refetching all transactions");
                    self.full_fetch(&outbound).await;
                }
            }
        }
    }

    /// Spawns a task that fetches all current transactions, then subscribes
    /// to the SSE feed for real-time updates, refetching on each new block
    /// environment.
    pub fn spawn(self) -> (mpsc::UnboundedReceiver<ReceivedTx>, JoinHandle<()>) {
        let (outbound, inbound) = mpsc::unbounded_channel();
        let jh = tokio::spawn(self.task_future(outbound));
        (inbound, jh)
    }
}

#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "only sent through an mpsc channel, which heap-allocates each message"
)]
pub enum ReceivedTx {
    Tx(TxEnvelope),
    StaleNonce,
}
