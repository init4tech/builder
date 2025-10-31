//! Tests for the block building task.

use alloy::{
    eips::BlockId,
    network::Ethereum,
    node_bindings::Anvil,
    primitives::U256,
    providers::{Provider, RootProvider},
    signers::local::PrivateKeySigner,
};
use builder::{
    tasks::{
        block::sim::Simulator,
        env::{EnvTask, Environment, SimEnv},
    },
    test_utils::{new_signed_tx, setup_logging, setup_test_config, test_block_env},
};
use signet_sim::SimCache;
use std::time::{Duration, Instant};

mod harness;
use harness::TestHarness;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_handle_build() {
    setup_logging();

    // Make a test config
    let config = setup_test_config().unwrap();

    // Create an anvil instance for testing
    let anvil_instance = Anvil::new().chain_id(signet_constants::pecorino::RU_CHAIN_ID).spawn();

    // Create a wallet
    let keys = anvil_instance.keys();
    let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
    let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

    // Create a rollup provider
    let ru_provider = RootProvider::<Ethereum>::new_http(anvil_instance.endpoint_url());
    let host_provider = config.connect_host_provider().await.unwrap();

    let block_env =
        EnvTask::new(config.clone(), host_provider.clone(), ru_provider.clone()).spawn().0;

    let block_builder =
        Simulator::new(&config, host_provider.clone(), ru_provider.clone(), block_env);

    // Setup a sim cache
    let sim_items = SimCache::new();

    // Add two transactions from two senders to the sim cache
    let tx_1 = new_signed_tx(&test_key_0, 0, U256::from(1_f64), 11_000).unwrap();
    sim_items.add_tx(tx_1, 0);

    let tx_2 = new_signed_tx(&test_key_1, 0, U256::from(2_f64), 10_000).unwrap();
    sim_items.add_tx(tx_2, 0);

    // Setup the block envs
    let finish_by = Instant::now() + Duration::from_secs(2);
    let ru_header = ru_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;
    let target_block_number = ru_header.number + 1;
    let target_timestamp = ru_header.timestamp + config.slot_calculator.slot_duration();

    assert!(target_timestamp > ru_header.timestamp);
    let block_env = test_block_env(config, target_block_number, 7, target_timestamp);

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_ticks_and_emits() {
    setup_logging();

    // Build harness
    let mut h = TestHarness::new().await.unwrap();

    // Prepare two senders and fund them if needed from anvil default accounts
    let keys = h.rollup.anvil().keys();
    let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());

    // Start simulator and tick a new SimEnv
    h.start();

    // Add a transaction into the sim cache
    h.add_tx(&test_key_0, 0, U256::from(1_u64), 11_000);

    // Tick host chain
    h.tick_from_host().await;

    // Expect a SimResult. Use the harness slot duration plus a small buffer so
    // we wait long enough for the simulator to complete heavy simulations.
    let wait = Duration::from_secs(h.config.slot_calculator.slot_duration() + 5);
    let got = h.recv_result(wait).await.expect("sim result");
    assert_eq!(got.block.tx_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_simulates_full_flow() {
    setup_logging();

    // Build harness
    let mut h = TestHarness::new().await.unwrap();

    // Prepare two senders and fund them if needed from anvil default accounts
    let keys = h.rollup.anvil().keys();
    let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
    let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

    // Add two transactions into the sim cache
    h.add_tx(&test_key_0, 0, U256::from(1_u64), 11_000);
    h.add_tx(&test_key_1, 0, U256::from(2_u64), 10_000);

    // Start simulator and tick a new SimEnv
    h.start();

    let (prev_ru_header, prev_host_header) = h.get_headers().await.unwrap();
    h.tick_from_headers(prev_ru_header, prev_host_header).await;

    // Expect a SimResult. Use the harness slot duration plus a small buffer.
    let wait = Duration::from_secs(h.config.slot_calculator.slot_duration() + 5);
    let got = h.recv_result(wait).await.expect("sim result");
    assert_eq!(got.block.tx_count(), 2);
}

/// Ensure the harness can manually advance the Anvil chain.
// #[ignore = "integration test"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_advances_anvil_chain() {
    setup_logging();
    let h = TestHarness::new().await.unwrap();

    let (rollup, host) = h.get_headers().await.unwrap();

    h.mine_blocks(2).await.expect("mine blocks");

    let (new_rollup, new_host) = h.get_headers().await.unwrap();
    assert_eq!(new_rollup.number, rollup.number + 2);
    assert_eq!(new_host.number, host.number + 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_stops() {
    setup_logging();
    let mut h = TestHarness::new().await.unwrap();

    h.start();
    h.stop().await;
}
