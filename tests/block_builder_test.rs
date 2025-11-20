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
        env::{Environment, SimEnv},
    },
    test_utils::{new_signed_tx_with_max_fee, setup_logging, setup_test_config, test_block_env},
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

    // Create a host provider
    let host_provider = config.connect_host_provider().await.unwrap();

    // Provide a dummy env receiver; this test calls handle_build directly and
    // doesn't use the env watch channel.
    let (_env_tx, env_rx) = tokio::sync::watch::channel(None);
    let block_builder = Simulator::new(&config, host_provider, ru_provider.clone(), env_rx);

    // Setup a sim cache
    let sim_items = SimCache::new();

    // Add two transactions from two senders to the sim cache
    let tx_1 =
        new_signed_tx_with_max_fee(&test_key_0, 0, U256::from(1_u64), 11_000, 10_000_000).unwrap();
    sim_items.add_tx(tx_1, 0);

    let tx_2 =
        new_signed_tx_with_max_fee(&test_key_1, 0, U256::from(2_u64), 10_000, 10_000_000).unwrap();
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
    h.start().await;

    // Add a transaction into the sim cache
    h.add_tx(&test_key_0, 0, U256::from(1_u64), 11_000);

    // Tick using the latest rollup and host headers
    h.mine_blocks(1).await.unwrap();

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
    h.start().await;

    h.mine_blocks(1).await.unwrap();

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

    h.mine_blocks(2).await.unwrap();

    let (new_rollup, new_host) = h.get_headers().await.unwrap();
    assert_eq!(new_rollup.number, rollup.number + 2);
    assert_eq!(new_host.number, host.number + 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_stops() {
    setup_logging();
    let mut h = TestHarness::new().await.unwrap();

    h.start().await;

    h.stop().await;

    assert_eq!(h.simulator_handle.is_none(), true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_timeout_without_results() {
    setup_logging();
    let mut h = TestHarness::new().await.unwrap();

    h.start().await;

    let wait = Duration::from_millis(250);
    let got = h.recv_result(wait).await;

    h.stop().await;

    assert!(got.is_none(), "expected timeout when no blocks are mined");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_start_is_idempotent() {
    setup_logging();
    let mut h = TestHarness::new().await.unwrap();

    h.start().await;
    let first_id = h.simulator_handle.as_ref().expect("simulator handle").id();

    h.start().await;
    let second_id = h.simulator_handle.as_ref().expect("simulator handle").id();

    h.stop().await;

    assert_eq!(first_id, second_id, "second start should reuse existing task");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_stop_without_start() {
    setup_logging();
    let mut h = TestHarness::new().await.unwrap();

    h.stop().await;

    assert!(h.simulator_handle.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_emits_multiple_results() {
    setup_logging();
    let mut h = TestHarness::new().await.unwrap();

    let keys = h.rollup.anvil().keys();
    let signer = PrivateKeySigner::from_signing_key(keys[0].clone().into());

    // First tick uses a transaction added before the simulator starts.
    h.add_tx(&signer, 0, U256::from(1_u64), 11_000);
    h.start().await;
    h.mine_blocks(1).await.unwrap();

    let wait = Duration::from_secs(h.config.slot_calculator.slot_duration() + 5);
    let first = h.recv_result(wait).await.expect("first sim result");
    assert_eq!(first.block.tx_count(), 1);

    // Second tick shouldn't need new transactions to emit a block.
    h.mine_blocks(1).await.unwrap();
    let second = h.recv_result(wait).await.expect("second sim result");
    assert_eq!(second.block.tx_count(), 0);
    assert_eq!(second.block.block_number(), first.block.block_number() + 1);

    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_harness_result_matches_headers() {
    setup_logging();
    let mut h = TestHarness::new().await.unwrap();

    let keys = h.rollup.anvil().keys();
    let signer = PrivateKeySigner::from_signing_key(keys[0].clone().into());

    // Capture the headers the harness should target.
    let (prev_rollup, prev_host) = h.get_headers().await.unwrap();

    h.add_tx(&signer, 0, U256::from(1_u64), 11_000);
    h.start().await;
    h.mine_blocks(1).await.unwrap();

    let wait = Duration::from_secs(h.config.slot_calculator.slot_duration() + 5);
    let got = h.recv_result(wait).await.expect("sim result");

    assert_eq!(got.block.tx_count(), 1);
    assert_eq!(got.rollup_block_number(), prev_rollup.number + 2);
    assert_eq!(got.host_block_number(), prev_host.number + 2);
    assert_eq!(got.prev_rollup().number, prev_rollup.number + 1);
    assert_eq!(got.prev_host().number, prev_host.number + 1);

    h.stop().await;
}
