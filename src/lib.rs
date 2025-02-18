#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![deny(unused_must_use, rust_2018_idioms)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

pub mod config;
pub mod service;
pub mod signer;
pub mod tasks;
pub mod utils;

use openssl as _;
