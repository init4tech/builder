//! Test utilities for testing builder tasks
use crate::{config::BuilderConfig, consts::PECORINO_CHAIN_ID};
use alloy::{
    consensus::{SignableTransaction, TxEip1559, TxEnvelope},
    primitives::{Address, TxKind, U256},
    signers::{SignerSync, local::PrivateKeySigner},
};
use eyre::Result;
use std::str::FromStr;
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// Sets up a block builder with test values
pub fn setup_test_config() -> Result<BuilderConfig> {
    let config = BuilderConfig {
        host_chain_id: 17000,
        ru_chain_id: 17001,
        host_rpc_url: "host-rpc.example.com".into(),
        ru_rpc_url: "ru-rpc.example.com".into(),
        tx_broadcast_urls: vec!["http://localhost:9000".into()],
        zenith_address: Address::default(),
        quincey_url: "http://localhost:8080".into(),
        builder_port: 8080,
        sequencer_key: None,
        builder_key: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        block_confirmation_buffer: 1,
        chain_offset: 0,
        target_slot_time: 1,
        builder_rewards_address: Address::default(),
        rollup_block_gas_limit: 100_000,
        tx_pool_url: "http://localhost:9000/".into(),
        tx_pool_cache_duration: 5,
        oauth_client_id: "some_client_id".into(),
        oauth_client_secret: "some_client_secret".into(),
        oauth_authenticate_url: "http://localhost:8080".into(),
        oauth_token_url: "http://localhost:8080".into(),
        oauth_token_refresh_interval: 300, // 5 minutes
        builder_helper_address: Address::default(),
        concurrency_limit: 1000,
        start_timestamp: 1740681556, // pecorino start timestamp as sane default
    };
    Ok(config)
}

/// Returns a new signed test transaction with the provided nonce, value, and mpfpg.
pub fn new_signed_tx(
    wallet: &PrivateKeySigner,
    nonce: u64,
    value: U256,
    mpfpg: u128,
) -> Result<TxEnvelope> {
    let tx = TxEip1559 {
        chain_id: PECORINO_CHAIN_ID,
        nonce,
        max_fee_per_gas: 50_000,
        max_priority_fee_per_gas: mpfpg,
        to: TxKind::Call(Address::from_str("0x0000000000000000000000000000000000000000").unwrap()),
        value,
        gas_limit: 50_000,
        ..Default::default()
    };
    let signature = wallet.sign_hash_sync(&tx.signature_hash())?;
    Ok(TxEnvelope::Eip1559(tx.into_signed(signature)))
}

/// Initializes a logger that prints during testing
pub fn setup_logging() {
    // Initialize logging
    let filter = EnvFilter::from_default_env();
    let fmt = tracing_subscriber::fmt::layer().with_filter(filter);
    let registry = tracing_subscriber::registry().with(fmt);
    let _ = registry.try_init();
}
