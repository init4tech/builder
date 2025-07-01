//! Tests for the block building task.

use alloy::{
    network::Ethereum,
    node_bindings::Anvil,
    primitives::U256,
    providers::{Provider, RootProvider},
    signers::local::PrivateKeySigner,
};
use builder::{
    tasks::block::sim::Simulator,
    test_utils::{new_signed_tx, setup_logging, setup_test_config, test_block_env},
};
use signet_sim::SimCache;
use signet_types::constants::SignetSystemConstants;
use std::time::{Duration, Instant};

/// Tests the `handle_build` method of the `Simulator`.
///
/// This test sets up a simulated environment using Anvil, creates a block builder,
/// and verifies that the block builder can successfully build a block containing
/// transactions from multiple senders.
#[ignore = "integration test"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_handle_build() {
    use alloy::eips::BlockId;

    setup_logging();

    // Make a test config
    let config = setup_test_config().unwrap();
    let constants = SignetSystemConstants::pecorino();

    // Create an anvil instance for testing
    let anvil_instance = Anvil::new().chain_id(signet_constants::pecorino::RU_CHAIN_ID).spawn();

    // Create a wallet
    let keys = anvil_instance.keys();
    let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
    let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

    // Create a rollup provider
    let ru_provider = RootProvider::<Ethereum>::new_http(anvil_instance.endpoint_url());

    let block_env = config.env_task().spawn().0;

    let block_builder = Simulator::new(&config, ru_provider.clone(), block_env);

    // Setup a sim cache
    let sim_items = SimCache::new();

    // Add two transactions from two senders to the sim cache
    let tx_1 = new_signed_tx(&test_key_0, 0, U256::from(1_f64), 11_000).unwrap();
    sim_items.add_tx(tx_1, 0);

    let tx_2 = new_signed_tx(&test_key_1, 0, U256::from(2_f64), 10_000).unwrap();
    sim_items.add_tx(tx_2, 0);

    // Setup the block env
    let finish_by = Instant::now() + Duration::from_secs(2);
    let header = ru_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;
    let number = header.number + 1;
    let timestamp = header.timestamp + config.slot_calculator.slot_duration();
    let block_env = test_block_env(config, number, 7, timestamp);

    // Spawn the block builder task
    let got = block_builder.handle_build(constants, sim_items, finish_by, block_env).await;

    // Assert on the built block
    assert!(got.is_ok());
    assert!(got.unwrap().tx_count() == 2);
}
