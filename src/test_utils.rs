//! Test utilities for testing builder tasks
use crate::config::BuilderConfig;
use alloy::{
    consensus::{SignableTransaction, TxEip1559, TxEnvelope},
    primitives::{Address, B256, TxKind, U256},
    rpc::client::BuiltInConnectionString,
    signers::{SignerSync, local::PrivateKeySigner},
};
use eyre::Result;
use init4_bin_base::{
    deps::tracing_subscriber::{
        EnvFilter, Layer, fmt, layer::SubscriberExt, registry, util::SubscriberInitExt,
    },
    perms::OAuthConfig,
    utils::{calc::SlotCalculator, provider::ProviderConfig},
};
use signet_constants::SignetSystemConstants;
use std::env;
use std::str::FromStr;
use trevm::revm::{context::BlockEnv, context_interface::block::BlobExcessGasAndPrice};

/// Sets up a block builder with test values
pub fn setup_test_config() -> Result<BuilderConfig> {
    let config = BuilderConfig {
        // host_chain_id: signet_constants::pecorino::HOST_CHAIN_ID,
        host_rpc: "ws://host-rpc.pecorino.signet.sh"
            .parse::<BuiltInConnectionString>()
            .map(ProviderConfig::new)
            .unwrap(),
        ru_rpc: "ws://rpc.pecorino.signet.sh"
            .parse::<BuiltInConnectionString>()
            .unwrap()
            .try_into()
            .unwrap(),
        tx_broadcast_urls: vec!["http://localhost:9000".into()],
        flashbots_endpoint: Some("https://relay-sepolia.flashbots.net:443".parse().unwrap()),
        zenith_address: Address::default(),
        quincey_url: "http://localhost:8080".into(),
        sequencer_key: None,
        builder_key: env::var("SEPOLIA_ETH_PRIV_KEY")
            .unwrap_or_else(|_| B256::repeat_byte(0x42).to_string()),
        builder_port: 8080,
        builder_rewards_address: Address::default(),
        rollup_block_gas_limit: 3_000_000_000,
        tx_pool_url: "http://localhost:9000/".parse().unwrap(),
        oauth: OAuthConfig {
            oauth_client_id: "some_client_id".into(),
            oauth_client_secret: "some_client_secret".into(),
            oauth_authenticate_url: "http://localhost:8080".parse().unwrap(),
            oauth_token_url: "http://localhost:8080".parse().unwrap(),
            oauth_token_refresh_interval: 300, // 5 minutes
        },
        concurrency_limit: None, // NB: Defaults to available parallelism
        slot_calculator: SlotCalculator::new(
            1740681556, // pecorino start timestamp as sane default
            0, 1,
        ),
        max_host_gas_coefficient: Some(80),
        constants: SignetSystemConstants::pecorino(),
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
        chain_id: 11155111,
        nonce,
        max_fee_per_gas: 10_000_000,
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
    let fmt = fmt::layer().with_filter(filter);
    let registry = registry().with(fmt);
    let _ = registry.try_init();
}

/// Returns a Pecorino block environment for simulation with the timestamp set to `finish_by`,
/// the block number set to latest + 1, system gas configs, and a beneficiary address.
pub fn test_block_env(
    config: BuilderConfig,
    number: u64,
    basefee: u64,
    timestamp: u64,
) -> BlockEnv {
    BlockEnv {
        number: U256::from(number),
        beneficiary: Address::repeat_byte(1),
        timestamp: U256::from(timestamp),
        gas_limit: config.rollup_block_gas_limit,
        basefee,
        difficulty: U256::ZERO,
        prevrandao: Some(B256::random()),
        blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
            excess_blob_gas: 0,
            blob_gasprice: 0,
        }),
    }
}
