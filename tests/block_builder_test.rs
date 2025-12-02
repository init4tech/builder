//! Tests for the block building task.

use alloy::{
    eips::{BlockId, Encodable2718},
    node_bindings::Anvil,
    primitives::U256,
    providers::Provider,
    rpc::types::mev::EthSendBundle,
    signers::local::PrivateKeySigner,
};
use builder::{
    tasks::{
        block::sim::SimulatorTask,
        env::{EnvTask, Environment, SimEnv},
    },
    test_utils::{new_signed_tx, setup_logging, setup_test_config, test_block_env},
};

use signet_bundle::SignetEthBundle;
use signet_constants::pecorino::RU_CHAIN_ID;
use signet_sim::SimCache;
use tokio::sync::{mpsc::unbounded_channel, watch::channel};

use std::{
    str::FromStr,
    time::{Duration, Instant},
};

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
    let anvil_instance = Anvil::new().chain_id(signet_constants::pecorino::RU_CHAIN_ID).spawn();

    // Create a wallet
    let keys = anvil_instance.keys();
    let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
    let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

    let block_env = EnvTask::new().await.unwrap().spawn().0;

    let block_builder = SimulatorTask::new(block_env).await.unwrap();

    // Setup a sim cache
    let sim_items = SimCache::new();

    // Add two transactions from two senders to the sim cache
    let tx_1 = new_signed_tx(&test_key_0, RU_CHAIN_ID, 0, U256::from(1_f64), 11_000).unwrap();
    sim_items.add_tx(tx_1, 0);

    let tx_2 = new_signed_tx(&test_key_1, RU_CHAIN_ID, 0, U256::from(2_f64), 10_000).unwrap();
    sim_items.add_tx(tx_2, 0);

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bundle_host_txns() {
    setup_logging();
    setup_test_config();
    
    let constants = builder::config().constants.clone();

    // Setup a host and rollup provider from the test config
    let host_provider = builder::config().connect_host_provider().await.unwrap();
    let host_chain_id = host_provider.get_chain_id().await.expect("gets host chain id");
    assert!(host_chain_id == constants.host_chain_id());
    

    let ru_provider = builder::config().connect_ru_provider().await.unwrap();
    let ru_chain_id = ru_provider.get_chain_id().await.expect("gets ru chain id");
    assert!(ru_chain_id == constants.ru_chain_id());

    let priv_key =
        std::env::var("SIGNET_WALLET_TEST_PK").expect("SIGNET_WALLET_TEST_PK env var to be set");
    let test_key = PrivateKeySigner::from_str(&priv_key).expect("valid private key");

    // NB: Test uses the same key for both host and rollup for simplicity.
    let test_key =
        PrivateKeySigner::from_bytes(&test_key.to_bytes()).expect("test key 0 to be set");

    // Setup the simulation environments
    let env = EnvTask::new().await.unwrap();
    let host_previous =
        host_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;
    let ru_previous = ru_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;
    let host_env = env.construct_host_env(host_previous);
    let ru_env = env.construct_rollup_env(ru_previous);

    let sim_env = SimEnv { host: host_env, rollup: ru_env, span: tracing::Span::none() };
    let (sim_tx, sim_rx) = channel::<Option<SimEnv>>(None);

    // Create a host and rollup transaction
    let ru_txn = new_signed_tx(&test_key, ru_chain_id, 0, U256::from(1), 10_000)
        .unwrap()
        .encoded_2718()
        .into();

    let host_txn = new_signed_tx(&test_key, host_chain_id, 1, U256::from(1), 10_000)
        .unwrap()
        .encoded_2718()
        .into();

    // Make a bundle out of them
    let bundle = SignetEthBundle {
        bundle: EthSendBundle {
            replacement_uuid: Some("test-replacement-uuid".to_string()),
            txs: vec![ru_txn],
            block_number: sim_env.rollup_block_number(),
            ..Default::default()
        },
        host_txs: vec![host_txn],
    };

    // Add it to the sim cache
    let sim_items = SimCache::new();
    sim_items.add_bundle(bundle, 7).expect("adds bundle");

    // Setup the simulator environment
    let (submit_channel, mut submit_receiver) = unbounded_channel();
    let simulator_task = SimulatorTask::new(sim_rx).await.unwrap();
    let simulator_jh = simulator_task.spawn_simulator_task(sim_items, submit_channel);

    // Send a new environment to tick the block builder simulation loop off
    sim_tx.send(Some(sim_env)).unwrap();

    // Wait for a result and assert on it
    let got = submit_receiver.recv().await.expect("built block");
    dbg!(&got.block);
    assert_eq!(got.block.transactions().len(), 1);
    assert_eq!(got.block.host_transactions().len(), 1);

    // Cleanup
    simulator_jh.abort();
}
