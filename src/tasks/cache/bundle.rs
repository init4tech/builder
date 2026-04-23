//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::{config::BuilderConfig, tasks::env::SimEnv};
use futures_util::{Stream, StreamExt, TryFutureExt, TryStreamExt};
use init4_bin_base::perms::tx_cache::{BuilderTxCache, BuilderTxCacheError};
use signet_tx_cache::{TxCacheError, types::CachedBundle};
use std::{ops::ControlFlow, pin::Pin, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};
use tracing::{Instrument, debug, trace, trace_span, warn};

type SseStream = Pin<Box<dyn Stream<Item = Result<CachedBundle, BuilderTxCacheError>> + Send>>;

const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_secs(1);
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);

/// The BundlePoller fetches bundles from the tx-pool on startup and on each
/// block environment change, and subscribes to an SSE stream for real-time
/// delivery of new bundles in between.
#[derive(Debug)]
pub struct BundlePoller {
    /// The builder configuration values.
    config: &'static BuilderConfig,
    /// Client for the tx cache.
    tx_cache: BuilderTxCache,
    /// Receiver for block environment updates, used to trigger refetches.
    envs: watch::Receiver<Option<SimEnv>>,
}

impl BundlePoller {
    /// Returns a new [`BundlePoller`] with the given block environment receiver.
    pub fn new(envs: watch::Receiver<Option<SimEnv>>) -> Self {
        let config = crate::config();
        let tx_cache = BuilderTxCache::new(config.tx_pool_url.clone(), config.oauth_token());
        Self { config, tx_cache, envs }
    }

    async fn full_fetch(&self, outbound: &mpsc::UnboundedSender<CachedBundle>) {
        let span = trace_span!("BundlePoller::full_fetch", url = %self.config.tx_pool_url);

        crate::metrics::inc_bundle_poll_count();
        if let Ok(bundles) = self
            .tx_cache
            .stream_bundles()
            .try_collect::<Vec<_>>()
            // NotOurSlot is expected whenever the builder isn't slot-permissioned;
            // don't bump the error counter or warn.
            .inspect_err(|error| match error {
                BuilderTxCacheError::TxCache(TxCacheError::NotOurSlot) => {
                    trace!("Not our slot to fetch bundles");
                }
                _ => {
                    crate::metrics::inc_bundle_poll_errors();
                    warn!(%error, "Failed to fetch bundles from tx-cache");
                }
            })
            .instrument(span.clone())
            .await
        {
            let _guard = span.entered();
            crate::metrics::record_bundles_fetched(bundles.len());
            trace!(count = bundles.len(), "found bundles");
            for bundle in bundles {
                if outbound.send(bundle).is_err() {
                    debug!("Outbound channel closed during full fetch");
                    return;
                }
            }
        }
    }

    /// Returns an empty stream on connection failure so the caller can handle
    /// reconnection uniformly.
    async fn subscribe(&self) -> SseStream {
        self.tx_cache
            .subscribe_bundles()
            .await
            .inspect(
                |_| debug!(url = %self.config.tx_pool_url, "SSE bundle subscription established"),
            )
            .inspect_err(|error| match error {
                BuilderTxCacheError::TxCache(TxCacheError::NotOurSlot) => {
                    trace!("Not our slot to subscribe to bundles");
                }
                _ => warn!(%error, "Failed to open SSE bundle subscription"),
            })
            .map(|s| Box::pin(s) as SseStream)
            .unwrap_or_else(|_| Box::pin(futures_util::stream::empty()))
    }

    /// Runs a full refetch concurrently with re-subscribing, to cover any
    /// items missed while disconnected.
    async fn reconnect(
        &mut self,
        outbound: &mpsc::UnboundedSender<CachedBundle>,
        backoff: &mut Duration,
    ) -> SseStream {
        tokio::select! {
            // Biased: a block env change wins over the backoff sleep. An env
            // change triggers a full refetch below anyway, which supersedes the
            // sleep-then-reconnect path — so there's no point waiting out the
            // backoff.
            biased;
            _ = self.envs.changed() => {}
            _ = time::sleep(*backoff) => {}
        }
        *backoff = (*backoff * 2).min(MAX_RECONNECT_BACKOFF);
        let (_, stream) = tokio::join!(self.full_fetch(outbound), self.subscribe());
        stream
    }

    /// Returns `Break` when the outbound channel has closed and the task
    /// should shut down.
    async fn handle_sse_item(
        &mut self,
        item: Option<Result<CachedBundle, BuilderTxCacheError>>,
        outbound: &mpsc::UnboundedSender<CachedBundle>,
        backoff: &mut Duration,
        stream: &mut SseStream,
    ) -> ControlFlow<()> {
        match item {
            Some(Ok(bundle)) => {
                *backoff = INITIAL_RECONNECT_BACKOFF;
                if outbound.send(bundle).is_err() {
                    trace!("No receivers left, shutting down");
                    return ControlFlow::Break(());
                }
            }
            Some(Err(error)) => {
                warn!(%error, "SSE bundle stream interrupted, reconnecting");
                *stream = self.reconnect(outbound, backoff).await;
            }
            None => {
                warn!("SSE bundle stream ended, reconnecting");
                *stream = self.reconnect(outbound, backoff).await;
            }
        }
        ControlFlow::Continue(())
    }

    async fn task_future(mut self, outbound: mpsc::UnboundedSender<CachedBundle>) {
        let (_, mut sse_stream) = tokio::join!(self.full_fetch(&outbound), self.subscribe());
        let mut backoff = INITIAL_RECONNECT_BACKOFF;

        loop {
            tokio::select! {
                item = sse_stream.next() => {
                    if self
                        .handle_sse_item(item, &outbound, &mut backoff, &mut sse_stream)
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
                    trace!("Block env changed, refetching all bundles");
                    self.full_fetch(&outbound).await;
                }
            }
        }
    }

    /// Spawns the task future and returns a receiver for bundles it finds.
    pub fn spawn(self) -> (mpsc::UnboundedReceiver<CachedBundle>, JoinHandle<()>) {
        let (outbound, inbound) = mpsc::unbounded_channel();
        let jh = tokio::spawn(self.task_future(outbound));
        (inbound, jh)
    }
}
