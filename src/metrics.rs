//! Centralized metrics for the builder.
//!
//! All metrics are defined and described here so that callers never need to
//! use bare `counter!` / `histogram!` macros directly. This prevents
//! metric-name typos and provides a single place to survey every metric the
//! builder emits.
use init4_bin_base::deps::metrics::{counter, describe_counter, describe_histogram, histogram};
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Metric names and help text
// ---------------------------------------------------------------------------

// -- Block building --

const BUILT_BLOCKS: &str = "signet.builder.built_blocks";
const BUILT_BLOCKS_HELP: &str = "Number of blocks built by the simulator.";

const BUILT_BLOCKS_TX_COUNT: &str = "signet.builder.built_blocks.tx_count";
const BUILT_BLOCKS_TX_COUNT_HELP: &str = "Transaction count per built block.";

// -- Cache polling --

const TX_NONCE_STALE: &str = "signet.builder.cache.tx_nonce_stale";
const TX_NONCE_STALE_HELP: &str = "Transactions dropped due to stale nonce.";

const TX_POLL_COUNT: &str = "signet.builder.cache.tx_poll_count";
const TX_POLL_COUNT_HELP: &str = "Transaction cache poll attempts.";

const TX_POLL_ERRORS: &str = "signet.builder.cache.tx_poll_errors";
const TX_POLL_ERRORS_HELP: &str = "Transaction cache poll errors.";

const TXS_FETCHED: &str = "signet.builder.cache.txs_fetched";
const TXS_FETCHED_HELP: &str = "Transactions fetched per poll cycle.";

const SSE_RECONNECT_ATTEMPTS: &str = "signet.builder.cache.sse_reconnect_attempts";
const SSE_RECONNECT_ATTEMPTS_HELP: &str = "SSE stream reconnect attempts.";

const SSE_SUBSCRIBE_ERRORS: &str = "signet.builder.cache.sse_subscribe_errors";
const SSE_SUBSCRIBE_ERRORS_HELP: &str = "SSE stream subscription failures.";

const BUNDLE_POLL_COUNT: &str = "signet.builder.cache.bundle_poll_count";
const BUNDLE_POLL_COUNT_HELP: &str = "Bundle cache poll attempts.";

const BUNDLE_POLL_ERRORS: &str = "signet.builder.cache.bundle_poll_errors";
const BUNDLE_POLL_ERRORS_HELP: &str = "Bundle cache poll errors.";

const BUNDLES_FETCHED: &str = "signet.builder.cache.bundles_fetched";
const BUNDLES_FETCHED_HELP: &str = "Bundles fetched per poll cycle.";

// -- Cache ingestion --

const CACHE_CLEANS: &str = "signet.builder.cache.cache_cleans";
const CACHE_CLEANS_HELP: &str = "Cache clean operations triggered by new block environments.";

const BUNDLES_SKIPPED: &str = "signet.builder.cache.bundles_skipped";
const BUNDLES_SKIPPED_HELP: &str = "Bundles skipped because they target a past block.";

const BUNDLE_ADD_ERRORS: &str = "signet.builder.cache.bundle_add_errors";
const BUNDLE_ADD_ERRORS_HELP: &str = "Errors adding bundles to the simulation cache.";

const BUNDLES_INGESTED: &str = "signet.builder.cache.bundles_ingested";
const BUNDLES_INGESTED_HELP: &str = "Bundles successfully ingested into the simulation cache.";

const TXS_INGESTED: &str = "signet.builder.cache.txs_ingested";
const TXS_INGESTED_HELP: &str = "Transactions successfully ingested into the simulation cache.";

const TX_RECOVER_FAILURES: &str = "signet.builder.cache.tx_recover_failures";
const TX_RECOVER_FAILURES_HELP: &str = "Transaction signature recovery failures.";

// -- Tx monitoring --

const TX_MINE_TIME: &str = "signet.builder.tx_mine_time";
const TX_MINE_TIME_HELP: &str = "Time in milliseconds for a submitted transaction to be mined.";

const TX_SUCCEEDED: &str = "signet.builder.tx_succeeded";
const TX_SUCCEEDED_HELP: &str = "Submitted transactions that succeeded on-chain.";

const TX_REVERTED: &str = "signet.builder.tx_reverted";
const TX_REVERTED_HELP: &str = "Submitted transactions that reverted on-chain.";

const TX_NOT_MINED: &str = "signet.builder.tx_not_mined";
const TX_NOT_MINED_HELP: &str = "Submitted transactions that were not mined within the timeout.";

const RPC_ERROR: &str = "signet.builder.rpc_error";
const RPC_ERROR_HELP: &str = "RPC errors encountered while watching submitted transactions.";

// -- Quincey signing --

const QUINCEY_SIGNATURES: &str = "signet.builder.quincey_signatures";
const QUINCEY_SIGNATURES_HELP: &str = "Successful Quincey signature responses.";

const QUINCEY_SIGNATURE_FAILURES: &str = "signet.builder.quincey_signature_failures";
const QUINCEY_SIGNATURE_FAILURES_HELP: &str = "Failed Quincey signature requests.";

// -- Submit --

const TRANSACTIONS_PREPARED: &str = "signet.builder.submit.transactions_prepared";
const TRANSACTIONS_PREPARED_HELP: &str = "Signed rollup block transactions ready for submission.";

const EMPTY_BLOCKS: &str = "signet.builder.submit.empty_blocks";
const EMPTY_BLOCKS_HELP: &str = "Empty blocks skipped during submission.";

const BUNDLE_PREP_FAILURES: &str = "signet.builder.submit.bundle_prep_failures";
const BUNDLE_PREP_FAILURES_HELP: &str = "Bundle preparation errors.";

const RELAY_SUBMISSIONS: &str = "signet.builder.submit.relay_submissions";
const RELAY_SUBMISSIONS_HELP: &str = "Relay submission attempts (incremented per relay).";

const DEADLINE_MISSED_BEFORE_SEND: &str = "signet.builder.submit.deadline_missed_before_send";
const DEADLINE_MISSED_BEFORE_SEND_HELP: &str =
    "Submissions that started after the slot deadline had already passed.";

const RELAY_SUCCESSES: &str = "signet.builder.submit.relay_successes";
const RELAY_SUCCESSES_HELP: &str = "Per-relay successful bundle submissions.";

const RELAY_FAILURES: &str = "signet.builder.submit.relay_failures";
const RELAY_FAILURES_HELP: &str = "Per-relay bundle submission errors.";

const RELAY_TIMEOUTS: &str = "signet.builder.submit.relay_timeouts";
const RELAY_TIMEOUTS_HELP: &str = "Per-relay bundle submission deadline timeouts.";

const ALL_RELAYS_FAILED: &str = "signet.builder.submit.all_relays_failed";
const ALL_RELAYS_FAILED_HELP: &str = "Bundles where no relay accepted the submission.";

const BUNDLES_SUBMITTED: &str = "signet.builder.submit.bundles_submitted";
const BUNDLES_SUBMITTED_HELP: &str = "Bundles accepted by at least one relay.";

const DEADLINE_MISSED: &str = "signet.builder.submit.deadline_missed";
const DEADLINE_MISSED_HELP: &str = "Bundles accepted by relays but after the slot deadline.";

const DEADLINE_MET: &str = "signet.builder.submit.deadline_met";
const DEADLINE_MET_HELP: &str = "Bundles accepted by relays within the slot deadline.";

// -- Pylon --

const PYLON_SUBMISSION_FAILURES: &str = "signet.builder.pylon.submission_failures";
const PYLON_SUBMISSION_FAILURES_HELP: &str = "Pylon sidecar submission errors.";

const PYLON_SIDECARS_SUBMITTED: &str = "signet.builder.pylon.sidecars_submitted";
const PYLON_SIDECARS_SUBMITTED_HELP: &str = "Successful Pylon sidecar submissions.";

// ---------------------------------------------------------------------------
// Lazy description registration
// ---------------------------------------------------------------------------

static DESCRIPTIONS: LazyLock<()> = LazyLock::new(|| {
    // Block building
    describe_counter!(BUILT_BLOCKS, BUILT_BLOCKS_HELP);
    describe_histogram!(BUILT_BLOCKS_TX_COUNT, BUILT_BLOCKS_TX_COUNT_HELP);

    // Cache polling
    describe_counter!(TX_NONCE_STALE, TX_NONCE_STALE_HELP);
    describe_counter!(TX_POLL_COUNT, TX_POLL_COUNT_HELP);
    describe_counter!(TX_POLL_ERRORS, TX_POLL_ERRORS_HELP);
    describe_histogram!(TXS_FETCHED, TXS_FETCHED_HELP);
    describe_counter!(SSE_RECONNECT_ATTEMPTS, SSE_RECONNECT_ATTEMPTS_HELP);
    describe_counter!(SSE_SUBSCRIBE_ERRORS, SSE_SUBSCRIBE_ERRORS_HELP);
    describe_counter!(BUNDLE_POLL_COUNT, BUNDLE_POLL_COUNT_HELP);
    describe_counter!(BUNDLE_POLL_ERRORS, BUNDLE_POLL_ERRORS_HELP);
    describe_histogram!(BUNDLES_FETCHED, BUNDLES_FETCHED_HELP);

    // Cache ingestion
    describe_counter!(CACHE_CLEANS, CACHE_CLEANS_HELP);
    describe_counter!(BUNDLES_SKIPPED, BUNDLES_SKIPPED_HELP);
    describe_counter!(BUNDLE_ADD_ERRORS, BUNDLE_ADD_ERRORS_HELP);
    describe_counter!(BUNDLES_INGESTED, BUNDLES_INGESTED_HELP);
    describe_counter!(TXS_INGESTED, TXS_INGESTED_HELP);
    describe_counter!(TX_RECOVER_FAILURES, TX_RECOVER_FAILURES_HELP);

    // Tx monitoring
    describe_histogram!(TX_MINE_TIME, TX_MINE_TIME_HELP);
    describe_counter!(TX_SUCCEEDED, TX_SUCCEEDED_HELP);
    describe_counter!(TX_REVERTED, TX_REVERTED_HELP);
    describe_counter!(TX_NOT_MINED, TX_NOT_MINED_HELP);
    describe_counter!(RPC_ERROR, RPC_ERROR_HELP);

    // Quincey signing
    describe_counter!(QUINCEY_SIGNATURES, QUINCEY_SIGNATURES_HELP);
    describe_counter!(QUINCEY_SIGNATURE_FAILURES, QUINCEY_SIGNATURE_FAILURES_HELP);

    // Submit
    describe_counter!(TRANSACTIONS_PREPARED, TRANSACTIONS_PREPARED_HELP);
    describe_counter!(EMPTY_BLOCKS, EMPTY_BLOCKS_HELP);
    describe_counter!(BUNDLE_PREP_FAILURES, BUNDLE_PREP_FAILURES_HELP);
    describe_counter!(RELAY_SUBMISSIONS, RELAY_SUBMISSIONS_HELP);
    describe_counter!(DEADLINE_MISSED_BEFORE_SEND, DEADLINE_MISSED_BEFORE_SEND_HELP);
    describe_counter!(RELAY_SUCCESSES, RELAY_SUCCESSES_HELP);
    describe_counter!(RELAY_FAILURES, RELAY_FAILURES_HELP);
    describe_counter!(RELAY_TIMEOUTS, RELAY_TIMEOUTS_HELP);
    describe_counter!(ALL_RELAYS_FAILED, ALL_RELAYS_FAILED_HELP);
    describe_counter!(BUNDLES_SUBMITTED, BUNDLES_SUBMITTED_HELP);
    describe_counter!(DEADLINE_MISSED, DEADLINE_MISSED_HELP);
    describe_counter!(DEADLINE_MET, DEADLINE_MET_HELP);

    // Pylon
    describe_counter!(PYLON_SUBMISSION_FAILURES, PYLON_SUBMISSION_FAILURES_HELP);
    describe_counter!(PYLON_SIDECARS_SUBMITTED, PYLON_SIDECARS_SUBMITTED_HELP);
});

/// Register all metric descriptions. Call once at startup (e.g. after
/// initializing the metrics recorder).
pub(crate) fn init() {
    LazyLock::force(&DESCRIPTIONS);
}

// ---------------------------------------------------------------------------
// Public API -- Block building
// ---------------------------------------------------------------------------

/// Increment the built-blocks counter.
pub(crate) fn inc_built_blocks() {
    counter!(BUILT_BLOCKS).increment(1);
}

/// Record the transaction count of a built block.
pub(crate) fn record_built_block_tx_count(count: usize) {
    histogram!(BUILT_BLOCKS_TX_COUNT).record(count as f64);
}

// ---------------------------------------------------------------------------
// Public API -- Cache polling
// ---------------------------------------------------------------------------

/// Increment the stale-nonce transaction counter.
pub(crate) fn inc_tx_nonce_stale() {
    counter!(TX_NONCE_STALE).increment(1);
}

/// Increment the transaction poll attempt counter.
pub(crate) fn inc_tx_poll_count() {
    counter!(TX_POLL_COUNT).increment(1);
}

/// Increment the transaction poll error counter.
pub(crate) fn inc_tx_poll_errors() {
    counter!(TX_POLL_ERRORS).increment(1);
}

/// Record the number of transactions fetched in a poll cycle.
pub(crate) fn record_txs_fetched(count: usize) {
    histogram!(TXS_FETCHED).record(count as f64);
}

/// Increment the SSE reconnect attempts counter.
pub(crate) fn inc_sse_reconnect_attempts() {
    counter!(SSE_RECONNECT_ATTEMPTS).increment(1);
}

/// Increment the SSE subscribe error counter.
pub(crate) fn inc_sse_subscribe_errors() {
    counter!(SSE_SUBSCRIBE_ERRORS).increment(1);
}

/// Increment the bundle poll attempt counter.
pub(crate) fn inc_bundle_poll_count() {
    counter!(BUNDLE_POLL_COUNT).increment(1);
}

/// Increment the bundle poll error counter.
pub(crate) fn inc_bundle_poll_errors() {
    counter!(BUNDLE_POLL_ERRORS).increment(1);
}

/// Record the number of bundles fetched in a poll cycle.
pub(crate) fn record_bundles_fetched(count: usize) {
    histogram!(BUNDLES_FETCHED).record(count as f64);
}

// ---------------------------------------------------------------------------
// Public API -- Cache ingestion
// ---------------------------------------------------------------------------

/// Increment the cache-clean counter.
pub(crate) fn inc_cache_cleans() {
    counter!(CACHE_CLEANS).increment(1);
}

/// Increment the skipped-bundle counter (bundle targets a past block).
pub(crate) fn inc_bundles_skipped() {
    counter!(BUNDLES_SKIPPED).increment(1);
}

/// Increment the bundle-add-error counter.
pub(crate) fn inc_bundle_add_errors() {
    counter!(BUNDLE_ADD_ERRORS).increment(1);
}

/// Increment the bundles-ingested counter.
pub(crate) fn inc_bundles_ingested() {
    counter!(BUNDLES_INGESTED).increment(1);
}

/// Increment the transactions-ingested counter.
pub(crate) fn inc_txs_ingested() {
    counter!(TXS_INGESTED).increment(1);
}

/// Increment the transaction signature recovery failure counter.
pub(crate) fn inc_tx_recover_failures() {
    counter!(TX_RECOVER_FAILURES).increment(1);
}

// ---------------------------------------------------------------------------
// Public API -- Tx monitoring
// ---------------------------------------------------------------------------

/// Record the time for a submitted transaction to be mined.
pub(crate) fn record_tx_mine_time(duration: std::time::Duration) {
    histogram!(TX_MINE_TIME).record(duration.as_millis() as f64);
}

/// Increment the succeeded-transaction counter.
pub(crate) fn inc_tx_succeeded() {
    counter!(TX_SUCCEEDED).increment(1);
}

/// Increment the reverted-transaction counter.
pub(crate) fn inc_tx_reverted() {
    counter!(TX_REVERTED).increment(1);
}

/// Increment the not-mined-transaction counter.
pub(crate) fn inc_tx_not_mined() {
    counter!(TX_NOT_MINED).increment(1);
}

/// Increment the RPC error counter.
pub(crate) fn inc_rpc_error() {
    counter!(RPC_ERROR).increment(1);
}

// ---------------------------------------------------------------------------
// Public API -- Quincey signing
// ---------------------------------------------------------------------------

/// Increment the successful Quincey signature counter.
pub(crate) fn inc_quincey_signatures() {
    counter!(QUINCEY_SIGNATURES).increment(1);
}

/// Increment the failed Quincey signature counter.
pub(crate) fn inc_quincey_signature_failures() {
    counter!(QUINCEY_SIGNATURE_FAILURES).increment(1);
}

// ---------------------------------------------------------------------------
// Public API -- Submit
// ---------------------------------------------------------------------------

/// Increment the transactions-prepared counter.
pub(crate) fn inc_transactions_prepared() {
    counter!(TRANSACTIONS_PREPARED).increment(1);
}

/// Increment the empty-blocks counter.
pub(crate) fn inc_empty_blocks() {
    counter!(EMPTY_BLOCKS).increment(1);
}

/// Increment the bundle-prep-failures counter.
pub(crate) fn inc_bundle_prep_failures() {
    counter!(BUNDLE_PREP_FAILURES).increment(1);
}

/// Increment the relay submission attempt counter by `count`.
pub(crate) fn inc_relay_submissions(count: usize) {
    counter!(RELAY_SUBMISSIONS).increment(count as u64);
}

/// Increment the deadline-missed-before-send counter.
pub(crate) fn inc_deadline_missed_before_send() {
    counter!(DEADLINE_MISSED_BEFORE_SEND).increment(1);
}

/// Increment the per-relay success counter.
pub(crate) fn inc_relay_successes() {
    counter!(RELAY_SUCCESSES).increment(1);
}

/// Increment the per-relay failure counter.
pub(crate) fn inc_relay_failures() {
    counter!(RELAY_FAILURES).increment(1);
}

/// Increment the per-relay timeout counter.
pub(crate) fn inc_relay_timeouts() {
    counter!(RELAY_TIMEOUTS).increment(1);
}

/// Increment the all-relays-failed counter.
pub(crate) fn inc_all_relays_failed() {
    counter!(ALL_RELAYS_FAILED).increment(1);
}

/// Increment the bundles-submitted counter.
pub(crate) fn inc_bundles_submitted() {
    counter!(BUNDLES_SUBMITTED).increment(1);
}

/// Increment the deadline-missed counter.
pub(crate) fn inc_deadline_missed() {
    counter!(DEADLINE_MISSED).increment(1);
}

/// Increment the deadline-met counter.
pub(crate) fn inc_deadline_met() {
    counter!(DEADLINE_MET).increment(1);
}

// ---------------------------------------------------------------------------
// Public API -- Pylon
// ---------------------------------------------------------------------------

/// Increment the Pylon submission failure counter.
pub(crate) fn inc_pylon_submission_failures() {
    counter!(PYLON_SUBMISSION_FAILURES).increment(1);
}

/// Increment the Pylon sidecars-submitted counter.
pub(crate) fn inc_pylon_sidecars_submitted() {
    counter!(PYLON_SIDECARS_SUBMITTED).increment(1);
}
