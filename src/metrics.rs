//! Builder metrics definitions
//!
//! This module centralizes all metric definitions for the builder.
//!
//! ## Counters
//! - Quincey signature requests (success/failure)
//! - Flashbots submissions (success/failure)
//! - Block builds
//! - Transaction outcomes
//!
//! ## Histograms
//! - Quincey signature duration
//! - Nonce fetch duration
//! - Transaction mine time
//! - Block transaction count

use init4_bin_base::deps::metrics::{
    Counter, Histogram, counter, describe_counter, describe_histogram, histogram,
};
use std::sync::LazyLock;

// -- Quincey --
const QUINCEY_SIGNATURES: &str = "signet.builder.quincey_signatures";
const QUINCEY_SIGNATURES_HELP: &str = "Number of successful Quincey signature requests";

const QUINCEY_SIGNATURE_FAILURES: &str = "signet.builder.quincey_signature_failures";
const QUINCEY_SIGNATURE_FAILURES_HELP: &str = "Number of failed Quincey signature requests";

const QUINCEY_SIGNATURE_DURATION_MS: &str = "signet.builder.quincey_signature_duration_ms";
const QUINCEY_SIGNATURE_DURATION_MS_HELP: &str =
    "Duration of Quincey signature requests in milliseconds";

// -- Nonce --
const NONCE_FETCH_DURATION_MS: &str = "signet.builder.nonce_fetch_duration_ms";
const NONCE_FETCH_DURATION_MS_HELP: &str = "Duration of nonce fetch requests in milliseconds";

// -- Flashbots --
const FLASHBOTS_BUNDLES_SUBMITTED: &str = "signet.builder.flashbots.bundles_submitted";
const FLASHBOTS_BUNDLES_SUBMITTED_HELP: &str =
    "Number of bundles successfully submitted to Flashbots";

const FLASHBOTS_SUBMISSION_FAILURES: &str = "signet.builder.flashbots.submission_failures";
const FLASHBOTS_SUBMISSION_FAILURES_HELP: &str = "Number of failed Flashbots bundle submissions";

const FLASHBOTS_BUNDLE_PREP_FAILURES: &str = "signet.builder.flashbots.bundle_prep_failures";
const FLASHBOTS_BUNDLE_PREP_FAILURES_HELP: &str = "Number of failed bundle preparations";

const FLASHBOTS_EMPTY_BLOCKS: &str = "signet.builder.flashbots.empty_block";
const FLASHBOTS_EMPTY_BLOCKS_HELP: &str = "Number of empty blocks skipped";

const FLASHBOTS_SUBMISSIONS: &str = "signet.builder.flashbots.submissions";
const FLASHBOTS_SUBMISSIONS_HELP: &str = "Number of submission attempts to Flashbots";

// -- Block Building --
const BUILT_BLOCKS: &str = "signet.builder.built_blocks";
const BUILT_BLOCKS_HELP: &str = "Number of blocks built by the simulator";

const BUILT_BLOCKS_TX_COUNT: &str = "signet.builder.built_blocks.tx_count";
const BUILT_BLOCKS_TX_COUNT_HELP: &str = "Number of transactions in built blocks";

// -- Transaction Outcomes --
const TX_MINE_TIME_MS: &str = "signet.builder.tx_mine_time_ms";
const TX_MINE_TIME_MS_HELP: &str = "Time for transaction to be mined in milliseconds";

const TX_SUCCEEDED: &str = "signet.builder.tx_succeeded";
const TX_SUCCEEDED_HELP: &str = "Number of transactions that succeeded";

const TX_REVERTED: &str = "signet.builder.tx_reverted";
const TX_REVERTED_HELP: &str = "Number of transactions that reverted";

const TX_NOT_MINED: &str = "signet.builder.tx_not_mined";
const TX_NOT_MINED_HELP: &str = "Number of transactions that timed out without being mined";

const RPC_ERROR: &str = "signet.builder.rpc_error";
const RPC_ERROR_HELP: &str = "Number of RPC errors encountered";

static DESCRIBE: LazyLock<()> = LazyLock::new(|| {
    // Quincey
    describe_counter!(QUINCEY_SIGNATURES, QUINCEY_SIGNATURES_HELP);
    describe_counter!(QUINCEY_SIGNATURE_FAILURES, QUINCEY_SIGNATURE_FAILURES_HELP);
    describe_histogram!(QUINCEY_SIGNATURE_DURATION_MS, QUINCEY_SIGNATURE_DURATION_MS_HELP);

    // Nonce
    describe_histogram!(NONCE_FETCH_DURATION_MS, NONCE_FETCH_DURATION_MS_HELP);

    // Flashbots
    describe_counter!(FLASHBOTS_BUNDLES_SUBMITTED, FLASHBOTS_BUNDLES_SUBMITTED_HELP);
    describe_counter!(FLASHBOTS_SUBMISSION_FAILURES, FLASHBOTS_SUBMISSION_FAILURES_HELP);
    describe_counter!(FLASHBOTS_BUNDLE_PREP_FAILURES, FLASHBOTS_BUNDLE_PREP_FAILURES_HELP);
    describe_counter!(FLASHBOTS_EMPTY_BLOCKS, FLASHBOTS_EMPTY_BLOCKS_HELP);
    describe_counter!(FLASHBOTS_SUBMISSIONS, FLASHBOTS_SUBMISSIONS_HELP);

    // Block building
    describe_counter!(BUILT_BLOCKS, BUILT_BLOCKS_HELP);
    describe_histogram!(BUILT_BLOCKS_TX_COUNT, BUILT_BLOCKS_TX_COUNT_HELP);

    // Transaction outcomes
    describe_histogram!(TX_MINE_TIME_MS, TX_MINE_TIME_MS_HELP);
    describe_counter!(TX_SUCCEEDED, TX_SUCCEEDED_HELP);
    describe_counter!(TX_REVERTED, TX_REVERTED_HELP);
    describe_counter!(TX_NOT_MINED, TX_NOT_MINED_HELP);
    describe_counter!(RPC_ERROR, RPC_ERROR_HELP);
});

// -- Quincey --

/// Counter for successful Quincey signature requests.
pub fn quincey_signatures() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(QUINCEY_SIGNATURES)
}

/// Counter for failed Quincey signature requests.
pub fn quincey_signature_failures() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(QUINCEY_SIGNATURE_FAILURES)
}

/// Histogram for Quincey signature request duration in milliseconds.
pub fn quincey_signature_duration_ms() -> Histogram {
    LazyLock::force(&DESCRIBE);
    histogram!(QUINCEY_SIGNATURE_DURATION_MS)
}

// -- Nonce --

/// Histogram for nonce fetch duration in milliseconds.
pub fn nonce_fetch_duration_ms() -> Histogram {
    LazyLock::force(&DESCRIBE);
    histogram!(NONCE_FETCH_DURATION_MS)
}

// -- Flashbots --

/// Counter for bundles successfully submitted to Flashbots.
pub fn flashbots_bundles_submitted() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(FLASHBOTS_BUNDLES_SUBMITTED)
}

/// Counter for failed Flashbots bundle submissions.
pub fn flashbots_submission_failures() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(FLASHBOTS_SUBMISSION_FAILURES)
}

/// Counter for failed bundle preparations.
pub fn flashbots_bundle_prep_failures() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(FLASHBOTS_BUNDLE_PREP_FAILURES)
}

/// Counter for empty blocks that were skipped.
pub fn flashbots_empty_blocks() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(FLASHBOTS_EMPTY_BLOCKS)
}

/// Counter for submission attempts to Flashbots.
pub fn flashbots_submissions() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(FLASHBOTS_SUBMISSIONS)
}

// -- Block Building --

/// Counter for blocks built by the simulator.
pub fn built_blocks() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(BUILT_BLOCKS)
}

/// Histogram for transaction count in built blocks.
pub fn built_blocks_tx_count() -> Histogram {
    LazyLock::force(&DESCRIBE);
    histogram!(BUILT_BLOCKS_TX_COUNT)
}

// -- Transaction Outcomes --

/// Histogram for transaction mine time in milliseconds.
pub fn tx_mine_time_ms() -> Histogram {
    LazyLock::force(&DESCRIBE);
    histogram!(TX_MINE_TIME_MS)
}

/// Counter for transactions that succeeded.
pub fn tx_succeeded() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(TX_SUCCEEDED)
}

/// Counter for transactions that reverted.
pub fn tx_reverted() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(TX_REVERTED)
}

/// Counter for transactions that timed out without being mined.
pub fn tx_not_mined() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(TX_NOT_MINED)
}

/// Counter for RPC errors encountered.
pub fn rpc_error() -> Counter {
    LazyLock::force(&DESCRIBE);
    counter!(RPC_ERROR)
}
