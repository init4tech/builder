//! Bundler service responsible for fetching bundles and sending them to the simulator.
use crate::config::BuilderConfig;
use futures_util::{
    TryFutureExt, TryStreamExt,
    future::Either,
    stream::{self, Stream, StreamExt},
};
use init4_bin_base::perms::SharedToken;
use signet_sim::{ProviderStateSource, SimItemValidity, check_bundle_tx_list};
use signet_tx_cache::{
    TxCache, TxCacheError,
    types::{BundleKey, BundleList, CacheResponse, CachedBundle},
};
use thiserror::Error;
use tokio::{
    sync::{
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
        watch,
    },
    task::JoinHandle,
    time::{self, Duration},
};
use tracing::{Instrument, debug_span, instrument, trace, trace_span, warn};

/// Errors that can occur when using the [`BundleClient`].
#[derive(Debug, Error)]
pub enum BundleCacheError {
    /// The background auth task has stopped.
    #[error("auth token unavailable (background auth task stopped): {0}")]
    TokenRetrieval(#[from] watch::error::RecvError),

    /// An error occurred during a TxCache operation.
    #[error(transparent)]
    TxCache(#[from] TxCacheError),
}

impl From<reqwest::Error> for BundleCacheError {
    fn from(err: reqwest::Error) -> Self {
        BundleCacheError::TxCache(TxCacheError::from(err))
    }
}

impl From<url::ParseError> for BundleCacheError {
    fn from(err: url::ParseError) -> Self {
        BundleCacheError::TxCache(TxCacheError::Url(err))
    }
}

/// Authenticated bundle client: wraps [`TxCache`] with a [`SharedToken`] for bearer auth.
#[derive(Debug, Clone)]
struct BundleClient {
    tx_cache: TxCache,
    token: SharedToken,
}

impl BundleClient {
    fn new(url: reqwest::Url, token: SharedToken) -> Self {
        Self { tx_cache: TxCache::new(url), token }
    }

    /// Fetch one page of bundles, optionally resuming from a cursor.
    #[instrument(skip(self))]
    async fn get_bundles(
        &self,
        query: Option<BundleKey>,
    ) -> Result<CacheResponse<BundleList>, BundleCacheError> {
        let url = self.tx_cache.url().join("bundles")?;
        let secret = self.token.secret().await?;
        self.tx_cache
            .client()
            .get(url)
            .query(&query)
            .bearer_auth(secret)
            .send()
            .await?
            .error_for_status()?
            .json::<CacheResponse<BundleList>>()
            .await
            .map_err(Into::into)
    }

    /// Stream all bundles, paginating automatically.
    fn stream_bundles(
        &self,
    ) -> impl Stream<Item = Result<CachedBundle, BundleCacheError>> + Send + '_ {
        stream::unfold(Some(None), move |cursor| async move {
            let cursor = cursor?;
            match self.get_bundles(cursor).await {
                Ok(response) => {
                    let (inner, next_cursor) = response.into_parts();
                    let bundles = stream::iter(inner.bundles).map(Ok);
                    Some((Either::Left(bundles), next_cursor.map(Some)))
                }
                Err(error) => Some((Either::Right(stream::once(async { Err(error) })), None)),
            }
        })
        .flatten()
    }
}

/// Poll interval for the bundle poller in milliseconds.
const POLL_INTERVAL_MS: u64 = 1000;

/// The BundlePoller polls the tx-pool for bundles.
#[derive(Debug)]
pub struct BundlePoller {
    /// The builder configuration values.
    config: &'static BuilderConfig,

    /// Client for the tx cache.
    tx_cache: BundleClient,

    /// Defines the interval at which the bundler polls the tx-pool for bundles.
    poll_interval_ms: u64,
}

impl Default for BundlePoller {
    fn default() -> Self {
        Self::new()
    }
}

/// Implements a poller for the block builder to pull bundles from the tx-pool.
impl BundlePoller {
    /// Creates a new BundlePoller from the provided builder config.
    pub fn new() -> Self {
        Self::new_with_poll_interval_ms(POLL_INTERVAL_MS)
    }

    /// Creates a new BundlePoller from the provided builder config and with the specified poll interval in ms.
    pub fn new_with_poll_interval_ms(poll_interval_ms: u64) -> Self {
        let config = crate::config();
        let tx_cache = BundleClient::new(config.tx_pool_url.clone(), config.oauth_token());
        Self { config, tx_cache, poll_interval_ms }
    }

    /// Returns the poll duration as a [`Duration`].
    const fn poll_duration(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms)
    }

    /// Fetches all bundles from the tx-cache, paginating through all available pages.
    pub async fn check_bundle_cache(&self) -> Result<Vec<CachedBundle>, BundleCacheError> {
        self.tx_cache.stream_bundles().try_collect().await
    }

    /// Spawns a tokio task to check the validity of all host transactions in a
    /// bundle before sending it to the cache task via the outbound channel.
    ///
    /// Uses [`check_bundle_tx_list`] from `signet-sim` to validate host tx nonces
    /// and balance against the host chain. Drops bundles that are not currently valid.
    fn spawn_check_bundle_nonces(bundle: CachedBundle, outbound: UnboundedSender<CachedBundle>) {
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

    async fn task_future(self, outbound: UnboundedSender<CachedBundle>) {
        loop {
            let span = trace_span!("BundlePoller::loop", url = %self.config.tx_pool_url);

            // Check this here to avoid making the web request if we know
            // we don't need the results.
            if outbound.is_closed() {
                span.in_scope(|| trace!("No receivers left, shutting down"));
                break;
            }

            crate::metrics::inc_bundle_poll_count();
            let Ok(bundles) = self
                .check_bundle_cache()
                .inspect_err(|error| match error {
                    BundleCacheError::TxCache(TxCacheError::NotOurSlot) => {
                        trace!("Not our slot to fetch bundles");
                    }
                    _ => {
                        crate::metrics::inc_bundle_poll_errors();
                        warn!(%error, "Failed to fetch bundles from tx-cache");
                    }
                })
                .instrument(span.clone())
                .await
            else {
                time::sleep(self.poll_duration()).await;
                continue;
            };

            {
                let _guard = span.entered();
                crate::metrics::record_bundles_fetched(bundles.len());
                trace!(count = bundles.len(), "fetched bundles from tx-cache");
                for bundle in bundles {
                    Self::spawn_check_bundle_nonces(bundle, outbound.clone());
                }
            }

            time::sleep(self.poll_duration()).await;
        }
    }

    /// Spawns a task that sends bundles it finds to its channel sender.
    pub fn spawn(self) -> (UnboundedReceiver<CachedBundle>, JoinHandle<()>) {
        let (outbound, inbound) = unbounded_channel();

        let jh = tokio::spawn(self.task_future(outbound));

        (inbound, jh)
    }
}
