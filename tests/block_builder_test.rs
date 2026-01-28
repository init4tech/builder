//! Tests for the block building task.

use alloy::{
    consensus::transaction::SignerRecoverable, eips::BlockId, node_bindings::Anvil,
    primitives::U256, providers::Provider, signers::local::PrivateKeySigner,
};
use builder::{
    tasks::{
        block::sim::SimulatorTask,
        env::{EnvTask, Environment, SimEnv},
    },
    test_utils::{new_signed_tx, setup_logging, setup_test_config, test_block_env},
};
use signet_sim::SimCache;
use std::time::{Duration, Instant};

/// Tests the `handle_build` method of the `SimulatorTask`.
///
/// This test sets up a simulated environment using Anvil, creates a block builder,
/// and verifies that the block builder can successfully build a block containing
/// transactions from multiple senders.
#[ignore = "integration test"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_handle_build() {
    setup_logging();

    // Make a test config
    let config = setup_test_config();

    // Create an anvil instance for testing
    let anvil_instance = Anvil::new().chain_id(signet_constants::parmigiana::RU_CHAIN_ID).spawn();

    // Create a wallet
    let keys = anvil_instance.keys();
    let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
    let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

    let block_env = EnvTask::new().await.unwrap().spawn().0;

    let block_builder = SimulatorTask::new(block_env).await.unwrap();

    // Setup a sim cache
    let sim_items = SimCache::new();

    // Add two transactions from two senders to the sim cache
    let tx_1 = new_signed_tx(&test_key_0, 0, U256::from(1_f64), 11_000).unwrap();
    let recovered_tx_1 = tx_1.try_into_recovered().unwrap();
    sim_items.add_tx(recovered_tx_1, 0);

    let tx_2 = new_signed_tx(&test_key_1, 0, U256::from(2_f64), 10_000).unwrap();
    let recovered_tx_2 = tx_2.try_into_recovered().unwrap();
    sim_items.add_tx(recovered_tx_2, 0);

    // Setup the block envs
    let finish_by = Instant::now() + Duration::from_secs(2);

    let ru_provider = builder::config().connect_ru_provider().await.unwrap();
    let ru_header = ru_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;
    let number = ru_header.number + 1;
    let timestamp = ru_header.timestamp + config.slot_calculator.slot_duration();
    let block_env = test_block_env(number, 7, timestamp);

    // Spawn the block builder task
    let sim_env = SimEnv {
        host: Environment::for_testing(),
        rollup: Environment::new(block_env, ru_header),
        span: tracing::Span::none(),
    };
    let got = block_builder.handle_build(sim_items, finish_by, &sim_env).await;

    // Assert on the built block
    assert!(got.is_ok());
    assert!(got.unwrap().tx_count() == 2);
}
