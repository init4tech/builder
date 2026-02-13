//! Builder binary components.

#![warn(
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    unreachable_pub,
    clippy::missing_const_for_fn,
    rustdoc::all
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![deny(unused_must_use, rust_2018_idioms)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![recursion_limit = "256"]

#[macro_use]
mod macros;

/// Configuration for the Builder binary.
pub mod config;

/// Quincey client for signing requests.
pub mod quincey;

/// Implements the `/healthcheck` endpoint.
pub mod service;

/// Actor-based tasks used to construct a builder.
pub mod tasks;

/// Utilities.
pub mod utils;

/// Test utilitites
pub mod test_utils;

use init4_bin_base::utils::from_env::FromEnv;
// Anonymous import suppresses warnings about unused imports.
use openssl as _;
use signet_constants::SignetSystemConstants;
use std::sync::OnceLock;

/// Global static configuration for the Builder binary.
pub static CONFIG: OnceLock<config::BuilderConfig> = OnceLock::new();

/// Load the Builder configuration from the environment and store it in the
/// global static CONFIG variable. Returns a reference to the configuration.
///
/// # Panics
///
/// Panics if the configuration cannot be loaded from the environment AND no
/// other configuration has been previously initialized.
pub fn config_from_env() -> &'static config::BuilderConfig {
    CONFIG.get_or_init(|| {
        config::BuilderConfig::from_env().expect("Failed to load Builder config").sanitize()
    })
}

/// Get a reference to the global Builder configuration.
///
/// # Panics
///
/// Panics if the configuration has not been initialized.
pub fn config() -> &'static config::BuilderConfig {
    CONFIG.get().expect("Builder config not initialized")
}

/// Get a reference to the Signet system constants from the global Builder
/// configuration.
///
/// # Panics
///
/// Panics if the configuration has not been initialized.
pub fn constants() -> &'static SignetSystemConstants {
    &config().constants
}
