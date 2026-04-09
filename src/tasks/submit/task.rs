//! Submit Task receives simulated blocks from an upstream channel and
//! submits them to configured MEV relay/builder endpoints as bundles.
//!
//! # Metrics
//!
//! | Name | Type | Description |
//! |------|------|-------------|
//! | `signet.builder.submit.transactions_prepared` | counter | Signed rollup block transactions ready for submission |
//! | `signet.builder.submit.empty_blocks` | counter | Empty blocks skipped |
//! | `signet.builder.submit.bundle_prep_failures` | counter | Bundle preparation errors |
//! | `signet.builder.submit.relay_submissions` | counter | Relay submission attempts (incremented per relay up front) |
//! | `signet.builder.submit.relay_successes` | counter | Per-relay successful submissions |
//! | `signet.builder.submit.relay_failures` | counter | Per-relay error responses |
//! | `signet.builder.submit.relay_timeouts` | counter | Per-relay deadline timeouts |
//! | `signet.builder.submit.all_relays_failed` | counter | No relay accepted the bundle |
//! | `signet.builder.submit.bundles_submitted` | counter | At least one relay accepted |
//! | `signet.builder.submit.deadline_met` | counter | Bundle accepted within slot deadline |
//! | `signet.builder.submit.deadline_missed` | counter | Bundle accepted but after slot deadline |
//! | `signet.builder.pylon.submission_failures` | counter | Pylon sidecar submission errors |
//! | `signet.builder.pylon.sidecars_submitted` | counter | Successful Pylon sidecar submissions |
use crate::{
    config::{BuilderConfig, HostProvider, PylonClient, RelayProvider, ZenithInstance},
    quincey::Quincey,
    tasks::{block::sim::SimResult, submit::SubmitPrep},
};
use alloy::{
    consensus::TxEnvelope,
    eips::Encodable2718,
    primitives::{Bytes, TxHash},
    providers::ext::MevApi,
    rpc::types::mev::EthSendBundle,
};
use futures_util::stream::{FuturesUnordered, StreamExt};
use init4_bin_base::utils::signer::LocalOrAws;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{join, sync::mpsc, task::JoinHandle};
use tracing::{Instrument, debug, debug_span, error, info, instrument, warn};

/// Handles preparation and submission of simulated rollup blocks to
/// configured MEV relay/builder endpoints as bundles.
///
/// Fans out each prepared bundle to all relays concurrently and submits
/// the blob sidecar to Pylon regardless of relay outcome.
#[derive(Debug)]
pub struct SubmitTask {
    /// Builder configuration for the task.
    config: &'static BuilderConfig,
    /// Quincey instance for block signing.
    quincey: Quincey,
    /// Zenith instance.
    zenith: ZenithInstance<HostProvider>,
    /// MEV relay/builder providers for bundle fan-out. Wrapped in [`Arc`]
    /// for cheap cloning into spawned submission tasks.
    relays: Arc<Vec<(url::Url, RelayProvider)>>,
    /// The key used to sign requests to MEV relays.
    signer: LocalOrAws,
    /// Channel for sending hashes of outbound transactions.
    outbound: mpsc::UnboundedSender<TxHash>,
    /// Pylon client for blob sidecar submission.
    pylon: PylonClient,
}

impl SubmitTask {
    /// Returns a new `SubmitTask` instance that receives `SimResult` types from the given
    /// channel and handles their preparation and submission to MEV relay/builder endpoints.
    pub async fn new(outbound: mpsc::UnboundedSender<TxHash>) -> eyre::Result<SubmitTask> {
        let config = crate::config();

        let (quincey, host_provider, relays, builder_key) = tokio::try_join!(
            config.connect_quincey(),
            config.connect_host_provider(),
            config.connect_relays(),
            config.connect_builder_signer()
        )?;

        info!(n_relays = relays.len(), "connected to MEV relay/builder endpoints");

        let zenith = config.connect_zenith(host_provider);
        let pylon = config.connect_pylon();

        Ok(Self {
            config,
            quincey,
            zenith,
            relays: Arc::new(relays),
            signer: builder_key,
            outbound,
            pylon,
        })
    }

    /// Prepares a MEV bundle from a simulation result.
    ///
    /// This function serves as an entry point for bundle preparation and is left
    /// for forward compatibility when adding different bundle preparation methods.
    pub async fn prepare(&self, sim_result: &SimResult) -> eyre::Result<EthSendBundle> {
        // This function is left for forwards compatibility when we want to add
        // different bundle preparation methods in the future.
        self.prepare_bundle(sim_result).await
    }

    /// Prepares a MEV bundle containing the host transactions and the rollup block.
    ///
    /// This method orchestrates the bundle preparation by:
    /// 1. Preparing and signing the submission transaction
    /// 2. Tracking the transaction hash for monitoring
    /// 3. Encoding the transaction for bundle inclusion
    /// 4. Constructing the complete bundle body
    #[instrument(skip_all, level = "debug")]
    async fn prepare_bundle(&self, sim_result: &SimResult) -> eyre::Result<EthSendBundle> {
        // Prepare and sign the transaction
        let block_tx = self.prepare_signed_transaction(sim_result).await?;

        // Track the outbound transaction
        self.track_outbound_tx(&block_tx);

        // Encode the transaction
        let tx_bytes = block_tx.encoded_2718().into();

        // Build the bundle body with the block_tx bytes as the last transaction in the bundle.
        let txs = sim_result.build_bundle_body(tx_bytes);

        // Create the MEV bundle (valid only in the specific host block)
        Ok(EthSendBundle {
            txs,
            block_number: sim_result.host_block_number(),
            ..Default::default()
        })
    }
    /// Prepares and signs the submission transaction for the rollup block.
    ///
    /// Creates a `SubmitPrep` instance to build the transaction, then fills
    /// and signs it using the host provider.
    #[instrument(skip_all, level = "debug")]
    async fn prepare_signed_transaction(&self, sim_result: &SimResult) -> eyre::Result<TxEnvelope> {
        let prep = SubmitPrep::new(
            &sim_result.block,
            self.host_provider(),
            self.quincey.clone(),
            self.config.clone(),
        );

        let tx = prep.prep_transaction(sim_result.prev_host()).await?;

        let sendable = self
            .host_provider()
            .fill(tx.into_request())
            .instrument(tracing::debug_span!("fill_tx").or_current())
            .await?;

        let tx_envelope = sendable.try_into_envelope()?;
        debug!(tx_hash = ?tx_envelope.hash(), "prepared signed rollup block transaction envelope");

        Ok(tx_envelope)
    }

    /// Tracks the outbound transaction hash and increments submission metrics.
    fn track_outbound_tx(&self, envelope: &TxEnvelope) {
        crate::metrics::inc_transactions_prepared();
        let hash = *envelope.tx_hash();
        if self.outbound.send(hash).is_err() {
            debug!("outbound channel closed, could not track tx hash");
        }
    }

    /// Main task loop that processes simulation results and submits bundles
    /// to all configured MEV relay/builder endpoints.
    ///
    /// For each `SimResult`:
    /// 1. Prepares the MEV bundle (once per block).
    /// 2. Fans out `send_bundle` to all relays concurrently with a
    ///    deadline timeout.
    /// 3. Submits the blob sidecar to Pylon unconditionally — even if all
    ///    relays fail or the deadline fires — so the sidecar is always
    ///    available for the host chain.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<SimResult>) {
        debug!("starting submit task with {} relay(s)", self.relays.len());

        loop {
            let Some(sim_result) = inbound.recv().await else {
                debug!("upstream task gone - exiting submit task");
                break;
            };

            let span = sim_result.clone_span();
            let deadline = self.calculate_submit_deadline();

            if sim_result.block.is_empty() {
                crate::metrics::inc_empty_blocks();
                span_debug!(span, "received empty block - skipping");
                continue;
            }
            span_debug!(span, "submit task received block");

            let bundle = match self.prepare(&sim_result).instrument(span.clone()).await {
                Ok(bundle) => bundle,
                Err(error) => {
                    crate::metrics::inc_bundle_prep_failures();
                    span_debug!(span, %error, "bundle preparation failed");
                    continue;
                }
            };

            // The block transaction is always last in the bundle.
            let block_tx = bundle.txs.last().unwrap().clone();

            let submission = Submission {
                bundle,
                block_tx,
                relays: Arc::clone(&self.relays),
                signer: self.signer.clone(),
                pylon: self.pylon.clone(),
                deadline,
            };

            let _guard = span.enter();
            let submit_span = debug_span!("submit.fan_out").or_current();
            tokio::spawn(submission.run().instrument(submit_span));
        }
    }

    /// Calculates the deadline for bundle submission.
    ///
    /// The deadline is calculated as the time remaining in the current slot,
    /// minus the configured submit deadline buffer. Submissions completing
    /// after this deadline will be logged as warnings.
    fn calculate_submit_deadline(&self) -> Instant {
        let slot_calculator = &self.config.slot_calculator;
        let timepoint_ms =
            slot_calculator.current_point_within_slot_ms().expect("host chain has started");

        let slot_duration = slot_calculator.slot_duration() * 1000;
        let submit_buffer = self.config.submit_deadline_buffer.into_inner();

        let remaining = slot_duration.saturating_sub(timepoint_ms).saturating_sub(submit_buffer);

        let deadline = Instant::now() + Duration::from_millis(remaining);
        deadline.max(Instant::now())
    }

    /// Returns a clone of the host provider for transaction operations.
    fn host_provider(&self) -> HostProvider {
        self.zenith.provider().clone()
    }

    /// Spawns the submit task in a new Tokio task.
    ///
    /// Returns a channel sender for submitting `SimResult`s and a join handle for the task.
    pub fn spawn(self) -> (mpsc::UnboundedSender<SimResult>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel::<SimResult>();
        let handle = tokio::spawn(self.task_future(inbound));
        (sender, handle)
    }
}

/// Outcome of a single relay submission attempt.
enum RelayOutcome {
    /// Relay accepted the bundle before the deadline.
    Success,
    /// Relay returned an error before the deadline.
    Failure,
    /// Relay did not respond before the per-relay deadline.
    Timeout,
}

const LATE_SUBMISSION_TIMEOUT: Duration = Duration::from_millis(1000);

/// State for a single bundle submission attempt across all relays and Pylon.
///
/// Created per-block by [`SubmitTask::task_future`] and spawned as an
/// independent tokio task so the main loop can immediately begin
/// preparing the next block.
struct Submission {
    /// The MEV bundle to fan out to relays.
    bundle: EthSendBundle,
    /// The encoded block transaction (last entry in the bundle), sent to
    /// Pylon as a blob sidecar.
    block_tx: Bytes,
    /// Relay endpoints for concurrent submission.
    relays: Arc<Vec<(url::Url, RelayProvider)>>,
    /// Signing key for relay authentication.
    signer: LocalOrAws,
    /// Pylon client for blob sidecar submission.
    pylon: PylonClient,
    /// Deadline by which relay responses should arrive.
    deadline: Instant,
}

impl Submission {
    /// Run the full submission pipeline: relay fan-out then Pylon sidecar.
    async fn run(self) {
        let (outcomes, ()) = join!(self.submit_to_relays(), self.submit_to_pylon());
        self.report_relay_metrics(&outcomes);
    }

    /// Submit the bundle to a single relay with an individual deadline.
    async fn submit_to_relay(
        bundle: EthSendBundle,
        signer: LocalOrAws,
        relay_url: &url::Url,
        provider: &RelayProvider,
        timeout_dur: Duration,
    ) -> RelayOutcome {
        let host: String = relay_url.host_str().unwrap_or("unknown").to_string();

        match tokio::time::timeout(
            timeout_dur,
            provider.send_bundle(bundle).with_auth(signer).into_future(),
        )
        .await
        {
            Ok(Ok(_)) => {
                debug!(relay = %host, "bundle accepted");
                RelayOutcome::Success
            }
            Ok(Err(err)) => {
                warn!(relay = %host, %err, "bundle rejected");
                RelayOutcome::Failure
            }
            Err(_) => {
                warn!(relay = %host, "relay submission timed out");
                RelayOutcome::Timeout
            }
        }
    }

    /// Fan out the bundle to all relays, processing results as they arrive.
    ///
    /// Each relay gets its own timeout so that a slow relay cannot suppress
    /// results from faster relays.
    async fn submit_to_relays(&self) -> Vec<RelayOutcome> {
        let n_relays = self.relays.len();
        crate::metrics::inc_relay_submissions(n_relays);

        let now = Instant::now();
        let timeout_dur = if now > self.deadline {
            let late_by = now.duration_since(self.deadline);
            warn!(?late_by, "submission started AFTER deadline; attempting anyway");
            crate::metrics::inc_deadline_missed_before_send();
            LATE_SUBMISSION_TIMEOUT
        } else {
            self.deadline.duration_since(now)
        };

        let mut futs: FuturesUnordered<_> = self
            .relays
            .iter()
            .map(|(url, provider)| {
                Self::submit_to_relay(
                    self.bundle.clone(),
                    self.signer.clone(),
                    url,
                    provider,
                    timeout_dur,
                )
            })
            .collect();

        let mut outcomes = Vec::with_capacity(n_relays);
        while let Some(outcome) = futs.next().await {
            outcomes.push(outcome);
        }

        outcomes
    }

    /// Report per-relay and aggregate metrics from the collected outcomes.
    fn report_relay_metrics(&self, outcomes: &[RelayOutcome]) {
        let n_relays = self.relays.len();
        let (mut successes, mut failures, mut timeouts) = (0u32, 0u32, 0u32);

        for outcome in outcomes {
            match outcome {
                RelayOutcome::Success => {
                    crate::metrics::inc_relay_successes();
                    successes += 1;
                }
                RelayOutcome::Failure => {
                    crate::metrics::inc_relay_failures();
                    failures += 1;
                }
                RelayOutcome::Timeout => {
                    crate::metrics::inc_relay_timeouts();
                    timeouts += 1;
                }
            }
        }

        if successes == 0 {
            crate::metrics::inc_all_relays_failed();
            error!(
                failures,
                timeouts, n_relays, "all relay submissions failed - bundle may not land"
            );
        } else {
            crate::metrics::inc_bundles_submitted();
            if Instant::now() > self.deadline {
                crate::metrics::inc_deadline_missed();
                warn!(successes, failures, timeouts, "bundle accepted by relays AFTER deadline");
            } else {
                crate::metrics::inc_deadline_met();
                info!(
                    successes,
                    failures, timeouts, n_relays, "bundle submitted to relays within deadline"
                );
            }
        }
    }

    /// Submit the blob sidecar to Pylon unconditionally.
    ///
    /// The sidecar must be available on the host chain even if relay
    /// submission failed or timed out.
    async fn submit_to_pylon(&self) {
        if let Err(err) = self.pylon.post_blob_tx(self.block_tx.clone()).await {
            crate::metrics::inc_pylon_submission_failures();
            warn!(%err, "pylon submission failed");
            return;
        }

        crate::metrics::inc_pylon_sidecars_submitted();
        debug!("posted sidecar to pylon");
    }
}
