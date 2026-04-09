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

/// Centralized metrics for the builder.
pub(crate) mod metrics;

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

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;

// Anonymous imports suppress warnings about unused crate dependencies.
use git_version as _;
use init4_bin_base::ConfigAndGuard;
use openssl as _;
use signet_constants::SignetSystemConstants;
use std::sync::OnceLock;

/// Global static configuration and OTLP guard for the Builder binary. The
/// guard is kept alive for the lifetime of the process.
static CONFIG_AND_GUARD: OnceLock<ConfigAndGuard<config::BuilderConfig>> = OnceLock::new();

/// Load the Builder configuration from the environment, initialize tracing and
/// metrics, and store the config in the global static. Returns a reference to
/// the configuration.
///
/// # Panics
///
/// Panics if the configuration cannot be loaded from the environment.
pub fn config_from_env() -> &'static config::BuilderConfig {
    &CONFIG_AND_GUARD
        .get_or_init(|| {
            let mut config_and_guard = init4_bin_base::init::<config::BuilderConfig>()
                .expect("Failed to load Builder config");
            config_and_guard.config = config_and_guard.config.sanitize();
            config_and_guard
        })
        .config
}

/// Get a reference to the global Builder configuration.
///
/// # Panics
///
/// Panics if the configuration has not been initialized.
pub fn config() -> &'static config::BuilderConfig {
    &CONFIG_AND_GUARD.get().expect("Builder config not initialized").config
}

/// Register all metric descriptions. Call once at startup after the metrics
/// recorder is installed.
pub fn init_metrics() {
    metrics::init();
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
