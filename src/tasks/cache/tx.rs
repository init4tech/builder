//! Transaction service responsible for fetching and sending transactions to the simulator.
use crate::{config::BuilderConfig, tasks::env::SimEnv};
use alloy::{
    consensus::{Transaction, TxEnvelope, transaction::SignerRecoverable},
    providers::Provider,
};
use backon::{BackoffBuilder, ExponentialBackoff, ExponentialBuilder};
use futures_util::{Stream, StreamExt, TryFutureExt, TryStreamExt};
use signet_tx_cache::{TxCache, TxCacheError};
use std::{ops::ControlFlow, pin::Pin, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};
use tracing::{Instrument, debug, debug_span, trace, trace_span, warn};

type SseStream = Pin<Box<dyn Stream<Item = Result<TxEnvelope, TxCacheError>> + Send>>;

const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_millis(250);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);

/// Builds the SSE reconnect backoff iterator.
///
/// Yields delays of 250ms, 500ms, 1s, 2s, 4s, 8s, 16s, 30s, 30s, … — doubling
/// each step, capped at [`MAX_RECONNECT_BACKOFF`] and unbounded in count.
/// Retries are intentionally unbounded: the per-block-env full refetch in
/// [`TxPoller::task_future`] is the floor on correctness, so SSE is a
/// best-effort latency optimization on top of it.
fn reconnect_backoff() -> ExponentialBackoff {
    ExponentialBuilder::default()
        .with_factor(2.0)
        .with_min_delay(INITIAL_RECONNECT_BACKOFF)
        .with_max_delay(MAX_RECONNECT_BACKOFF)
        .without_max_times()
        .build()
}

/// Fetches transactions from the transaction pool on startup and on each
/// block environment change, and subscribes to an SSE stream for real-time
/// delivery of new transactions in between.
#[derive(Debug)]
pub struct TxPoller {
    /// Config values from the Builder.
    config: &'static BuilderConfig,
    /// Client for the tx cache.
    tx_cache: TxCache,
    /// Receiver for block environment updates, used to trigger refetches.
    envs: watch::Receiver<Option<SimEnv>>,
    /// SSE reconnect backoff. Reconnect attempts are intentionally unbounded:
    /// this poller is a long-lived subscriber, so tx-cache outages should
    /// degrade delivery until recovery rather than permanently disabling it.
    reconnect_backoff: ExponentialBackoff,
}

impl TxPoller {
    /// Returns a new [`TxPoller`] with the given block environment receiver.
    pub fn new(envs: watch::Receiver<Option<SimEnv>>) -> Self {
        let config = crate::config();
        let tx_cache = TxCache::new(config.tx_pool_url.clone());
        Self { config, tx_cache, envs, reconnect_backoff: reconnect_backoff() }
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

    /// Pulls every transaction currently in the cache, paginating until the
    /// stream is exhausted. Pure fetch — no metrics, no dispatch.
    async fn check_tx_cache(&self) -> Result<Vec<TxEnvelope>, TxCacheError> {
        self.tx_cache.stream_transactions().try_collect().await
    }

    /// Fetches all transactions from the cache and dispatches each one to
    /// a nonce-check task. Records poll metrics around the fetch.
    async fn fetch_and_dispatch(&self, outbound: &mpsc::UnboundedSender<ReceivedTx>) {
        let span = trace_span!("TxPoller::fetch_and_dispatch", url = %self.config.tx_pool_url);

        crate::metrics::inc_tx_poll_count();
        let Ok(transactions) = self
            .check_tx_cache()
            .inspect_err(|error| {
                crate::metrics::inc_tx_poll_errors();
                debug!(%error, "Error fetching transactions");
            })
            .instrument(span.clone())
            .await
        else {
            return;
        };

        let _guard = span.entered();
        crate::metrics::record_txs_fetched(transactions.len());
        trace!(count = transactions.len(), "found transactions");
        for tx in transactions {
            self.spawn_check_nonce(tx, outbound.clone());
        }
    }

    /// Opens an SSE subscription to the transaction feed. Returns an empty
    /// stream on connection failure so the caller can handle reconnection
    /// uniformly.
    async fn subscribe(&self) -> SseStream {
        self.tx_cache
            .subscribe_transactions()
            .await
            .inspect(|_| debug!(url = %self.config.tx_pool_url, "SSE transaction subscription established"))
            .inspect_err(|error| warn!(%error, "Failed to open SSE transaction subscription"))
            .map(|s| Box::pin(s) as SseStream)
            .unwrap_or_else(|_| Box::pin(futures_util::stream::empty()))
    }

    /// Reconnects the SSE stream with backoff. Performs a full refetch to
    /// cover any items missed while disconnected.
    async fn reconnect(&mut self, outbound: &mpsc::UnboundedSender<ReceivedTx>) -> SseStream {
        crate::metrics::inc_sse_reconnect_attempts();
        let delay = self.reconnect_backoff.next().expect("backoff is unbounded");
        tokio::select! {
            // Biased: a block env change wins over the backoff sleep. An env
            // change triggers a full refetch below anyway, which supersedes the
            // sleep-then-reconnect path — so there's no point waiting out the
            // backoff.
            biased;
            _ = self.envs.changed() => {}
            _ = time::sleep(delay) => {}
        }
        let (_, stream) = tokio::join!(self.fetch_and_dispatch(outbound), self.subscribe());
        stream
    }

    /// Processes a single item yielded by the SSE stream: dispatches the tx
    /// for nonce checking on success, or reconnects on error / stream end.
    /// Returns `Break` when the outbound channel has closed and the task
    /// should shut down.
    async fn handle_sse_item(
        &mut self,
        item: Option<Result<TxEnvelope, TxCacheError>>,
        outbound: &mpsc::UnboundedSender<ReceivedTx>,
        stream: &mut SseStream,
    ) -> ControlFlow<()> {
        match item {
            Some(Ok(tx)) => {
                if outbound.is_closed() {
                    trace!("No receivers left, shutting down");
                    return ControlFlow::Break(());
                }
                self.reconnect_backoff = reconnect_backoff();
                self.spawn_check_nonce(tx, outbound.clone());
            }
            Some(Err(error)) => {
                warn!(%error, "SSE transaction stream error, reconnecting");
                *stream = self.reconnect(outbound).await;
            }
            None => {
                warn!("SSE transaction stream ended, reconnecting");
                *stream = self.reconnect(outbound).await;
            }
        }
        ControlFlow::Continue(())
    }

    async fn task_future(mut self, outbound: mpsc::UnboundedSender<ReceivedTx>) {
        // Initial full fetch of all currently-cached transactions, plus SSE
        // subscription for real-time delivery.
        let (_, mut sse_stream) =
            tokio::join!(self.fetch_and_dispatch(&outbound), self.subscribe());

        loop {
            tokio::select! {
                item = sse_stream.next() => {
                    if self
                        .handle_sse_item(item, &outbound, &mut sse_stream)
                        .await
                        .is_break()
                    {
                        break;
                    }
                }
                res = self.envs.changed() => {
                    if res.is_err() {
                        debug!("Block env channel closed, shutting down");
                        break;
                    }
                    debug!("Block env changed, refetching all transactions");
                    self.fetch_and_dispatch(&outbound).await;
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
