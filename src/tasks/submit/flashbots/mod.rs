//! Signet's Flashbots Block Submitter

/// handles rollup block submission to the Flashbots network
/// via the provider
pub mod submitter;
pub use submitter::FlashbotsSubmitter;

/// implements a bundle provider API for building Flashbots
/// compatible MEV bundles
pub mod provider;
pub use provider::FlashbotsProvider;

/// handles the lifecyle of receiving, preparing, and submitting
/// a rollup block to the Flashbots network.
pub mod task;
pub use task::FlashbotsTask;
