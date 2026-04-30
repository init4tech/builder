//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::{config::BuilderConfig, tasks::env::SimEnv};
use futures_util::{Stream, StreamExt, TryFutureExt, TryStreamExt};
use init4_bin_base::perms::tx_cache::{BuilderTxCache, BuilderTxCacheError};
use signet_sim::{ProviderStateSource, SimItemValidity, check_bundle_tx_list};
use signet_tx_cache::{TxCacheError, types::CachedBundle};
use std::{ops::ControlFlow, pin::Pin, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};
use tracing::{Instrument, debug, debug_span, trace, warn};

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

    /// Pulls every bundle currently in the cache, paginating until the stream
    /// is exhausted. Pure fetch — no metrics, no forwarding.
    async fn check_bundle_cache(&self) -> Result<Vec<CachedBundle>, BuilderTxCacheError> {
        self.tx_cache.stream_bundles().try_collect().await
    }

    /// Fetches all bundles from the cache and forwards each to the outbound
    /// channel. Records poll metrics around the fetch.
    async fn fetch_and_forward(&self, outbound: &mpsc::UnboundedSender<CachedBundle>) {
        crate::metrics::inc_bundle_poll_count();
        // NotOurSlot is expected whenever the builder isn't slot-permissioned;
        // don't bump the error counter or warn.
        let Ok(bundles) = self
            .check_bundle_cache()
            .inspect_err(|error| match error {
                BuilderTxCacheError::TxCache(TxCacheError::NotOurSlot) => {
                    trace!("Not our slot to fetch bundles");
                }
                _ => {
                    crate::metrics::inc_bundle_poll_errors();
                    warn!(%error, "Failed to fetch bundles from tx-cache");
                }
            })
            .await
        else {
            return;
        };

        crate::metrics::record_bundles_fetched(bundles.len());
        trace!(count = bundles.len(), "found bundles");
        for bundle in bundles {
            Self::spawn_check_bundle_nonces(bundle, outbound.clone());
        }
    }

    /// Spawns a tokio task to check the validity of all host transactions in a
    /// bundle before sending it to the cache task via the outbound channel.
    ///
    /// Uses [`check_bundle_tx_list`] from `signet-sim` to validate host tx nonces
    /// and balance against the host chain. Drops bundles that are not currently valid.
    fn spawn_check_bundle_nonces(
        bundle: CachedBundle,
        outbound: mpsc::UnboundedSender<CachedBundle>,
    ) {
        let span = debug_span!("check_bundle_nonces", bundle_id = %bundle.id);
        tokio::spawn(async move {
            let recovered = match bundle.bundle.try_to_recovered() {
                Ok(recovered) => recovered,
                Err(error) => {
                    span_debug!(span, ?error, "Failed to recover bundle, dropping");
                    return;
                }
            };

            if recovered.host_txs().is_empty() {
                let _ = outbound.send(bundle).inspect_err(|_| {
                    span_debug!(span, "Outbound channel closed");
                });
                return;
            }

            // Check if the receiver is still alive before doing expensive nonce validation over
            // the network.
            if outbound.is_closed() {
                span_debug!(span, "Outbound channel closed, skipping nonce validation");
                return;
            }

            let Ok(host_provider) =
                crate::config().connect_host_provider().instrument(span.clone()).await
            else {
                span_debug!(span, "Failed to connect to host provider, dropping bundle");
                return;
            };

            let source = ProviderStateSource(host_provider);
            match check_bundle_tx_list(recovered.host_tx_reqs(), &source)
                .instrument(span.clone())
                .await
            {
                Ok(SimItemValidity::Now) | Ok(SimItemValidity::Future) => {
                    let _ = outbound.send(bundle).inspect_err(|_| {
                        span_debug!(span, "Outbound channel closed");
                    });
                }
                Ok(SimItemValidity::Never) => {
                    span_debug!(span, "Dropping bundle: host txs will never be valid");
                }
                Err(error) => {
                    span_debug!(span, %error, "Failed to check bundle validity, dropping");
                }
            }
        });
    }

    /// Returns `None` on connection failure; the caller is responsible for
    /// scheduling a retry. Avoids the empty-stream sentinel pattern that
    /// would double-log "stream ended" on a failure that never opened.
    async fn subscribe(&self) -> Option<SseStream> {
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
                _ => {
                    crate::metrics::inc_sse_subscribe_errors();
                    warn!(%error, "Failed to open SSE bundle subscription");
                }
            })
            .ok()
            .map(|s| Box::pin(s) as SseStream)
    }

    /// Loops with exponential backoff until either a fresh SSE stream is
    /// established (returned as `Some`) or the outbound channel is closed
    /// (returned as `None`, signalling the task should shut down). Runs a
    /// full refetch alongside each subscribe attempt to cover items missed
    /// while disconnected.
    async fn reconnect(
        &mut self,
        outbound: &mpsc::UnboundedSender<CachedBundle>,
        backoff: &mut Duration,
    ) -> Option<SseStream> {
        loop {
            if outbound.is_closed() {
                return None;
            }
            crate::metrics::inc_sse_reconnect_attempts();
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
            let (_, stream) = tokio::join!(self.fetch_and_forward(outbound), self.subscribe());
            if let Some(stream) = stream {
                return Some(stream);
            }
        }
    }

    /// Reconnects and swaps in the fresh stream, or returns `Break` if the
    /// outbound channel closed during the reconnect loop.
    async fn try_reconnect(
        &mut self,
        outbound: &mpsc::UnboundedSender<CachedBundle>,
        backoff: &mut Duration,
        stream: &mut SseStream,
    ) -> ControlFlow<()> {
        match self.reconnect(outbound, backoff).await {
            Some(s) => {
                *stream = s;
                ControlFlow::Continue(())
            }
            None => ControlFlow::Break(()),
        }
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
                if outbound.is_closed() {
                    trace!("No receivers left, shutting down");
                    return ControlFlow::Break(());
                }
                Self::spawn_check_bundle_nonces(bundle, outbound.clone());
                ControlFlow::Continue(())
            }
            Some(Err(error)) => {
                warn!(%error, "SSE bundle stream interrupted, reconnecting");
                self.try_reconnect(outbound, backoff, stream).await
            }
            None => {
                warn!("SSE bundle stream ended, reconnecting");
                self.try_reconnect(outbound, backoff, stream).await
            }
        }
    }

    async fn task_future(mut self, outbound: mpsc::UnboundedSender<CachedBundle>) {
        let (_, sub) = tokio::join!(self.fetch_and_forward(&outbound), self.subscribe());
        let mut backoff = INITIAL_RECONNECT_BACKOFF;
        let mut sse_stream = match sub {
            Some(s) => s,
            None => match self.reconnect(&outbound, &mut backoff).await {
                Some(s) => s,
                None => return,
            },
        };

        loop {
            if outbound.is_closed() {
                debug!("Outbound channel closed, shutting down");
                break;
            }
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
                    // Run the refetch under the BlockConstruction span built by
                    // EnvTask, so its sim.ru.number / sim.host.number fields
                    // attach to anything the refetch logs.
                    let span = self
                        .envs
                        .borrow()
                        .as_ref()
                        .map_or_else(tracing::Span::none, |env| env.clone_span());
                    async {
                        debug!("Block env changed, refetching all bundles");
                        self.fetch_and_forward(&outbound).await;
                    }
                    .instrument(span)
                    .await;
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
