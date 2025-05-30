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

// Anonymous import suppresses warnings about unused imports.
use openssl as _;
