//! Signet's Flashbots Block Submitter

/// implements a bundle provider API for building Flashbots
/// compatible MEV bundles
pub mod provider;
pub use provider::Flashbots;

/// handles the lifecyle of receiving, preparing, and submitting
/// a rollup block to the Flashbots network.
pub mod task;
pub use task::FlashbotsTask;
