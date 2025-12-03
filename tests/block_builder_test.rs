//! Tests for the block building task.

use alloy::{
    eips::{BlockId, Encodable2718},
    node_bindings::{Anvil, AnvilInstance},
    primitives::{B256, U256},
    providers::{
        Provider, ProviderBuilder, RootProvider,
        fillers::{BlobGasFiller, SimpleNonceManager},
    },
    rpc::types::mev::EthSendBundle,
    signers::local::PrivateKeySigner,
};
use builder::{
    config::{HostProvider, RuProvider},
    constants,
    tasks::{
        block::sim::SimulatorTask,
        env::{EnvTask, SimEnv},
    },
    test_utils::{new_signed_tx, setup_logging, setup_test_config},
};

use signet_bundle::SignetEthBundle;
use signet_sim::SimCache;
use tokio::sync::{mpsc::unbounded_channel, watch::channel};

use std::time::{Duration, Instant};

/// Tests the `handle_build` method of the `SimulatorTask`.
///
/// This test sets up a simulated environment using Anvil, creates a block builder,
/// and verifies that the block builder can successfully build a block containing
/// transactions from multiple senders.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_handle_build() {
    setup_logging();
    setup_test_config();

    // Setup quincey
    let quincey = builder::config().connect_quincey().await.unwrap();

    // Setup host provider
    let (_host_anvil, host_provider, _host_signer) = spawn_host_anvil();

    // Setup rollup provider
    let (_ru_anvil, ru_provider, rollup_signers) = spawn_rollup_anvil();
    let mut rollup_signers = rollup_signers.into_iter();
    let rollup_key = rollup_signers.next().unwrap();
    let rollup_key_two = rollup_signers.next().unwrap();

    // Setup the env task and environments
    let env_task = EnvTask::new(host_provider.clone(), ru_provider.clone(), quincey).await.unwrap();
    let sim_env = latest_sim_env(&env_task, &host_provider, &ru_provider).await;

    let (block_env, _jh) = env_task.spawn();
    let block_builder = SimulatorTask::new(block_env, host_provider, ru_provider);

    // Setup a sim cache
    let sim_items = SimCache::new();

    // Add two transactions from two senders to the sim cache
    let tx_1 = new_signed_tx(&rollup_key, constants().ru_chain_id(), 0, U256::from(1_f64), 11_000)
        .unwrap();
    sim_items.add_tx(tx_1, 0);

    let tx_2 =
        new_signed_tx(&rollup_key_two, constants().ru_chain_id(), 0, U256::from(2_f64), 10_000)
            .unwrap();
    sim_items.add_tx(tx_2, 0);

    // Spawn the block builder task
    let finish_by = Instant::now() + Duration::from_secs(2);
    let got = block_builder.handle_build(sim_items, finish_by, &sim_env).await;

    // Assert on the built block
    assert!(got.is_ok());
    assert!(got.unwrap().tx_count() == 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bundle_host_txns() {
    setup_logging();
    setup_test_config();

    // Setup host provider
    let (_host_anvil, host_provider, host_signer) = spawn_host_anvil();
    let host_chain_id = host_provider.get_chain_id().await.expect("gets host chain id");
    assert_eq!(host_chain_id, constants().host_chain_id());

    // Setup rollup provider
    let (_ru_anvil, ru_provider, rollup_signers) = spawn_rollup_anvil();
    let rollup_signer = rollup_signers.into_iter().next().unwrap();
    let ru_chain_id = ru_provider.get_chain_id().await.expect("gets ru chain id");
    assert_eq!(ru_chain_id, constants().ru_chain_id());

    let quincey = builder::config().connect_quincey().await.unwrap();

    // Setup the simulation environments
    let env_task = EnvTask::new(host_provider.clone(), ru_provider.clone(), quincey).await.unwrap();
    let sim_env = latest_sim_env(&env_task, &host_provider, &ru_provider).await;

    // Create a simulation environment and plumbing
    let (sim_tx, sim_rx) = channel::<Option<SimEnv>>(None);

    // Create a host and rollup transaction
    let ru_txn = new_signed_tx(&rollup_signer, ru_chain_id, 0, U256::from(1), 10_000)
        .unwrap()
        .encoded_2718()
        .into();

    let host_txn = new_signed_tx(&host_signer, host_chain_id, 0, U256::from(1), 10_000)
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
    let (submit_tx, mut submit_rx) = unbounded_channel();
    let simulator_task = SimulatorTask::new(sim_rx, host_provider, ru_provider);
    let simulator_jh = simulator_task.spawn_simulator_task(sim_items, submit_tx);

    // Send a new environment to tick the block builder simulation loop off
    sim_tx.send(Some(sim_env)).unwrap();

    // Wait for a result and assert on it
    let got = submit_rx.recv().await.expect("built block");
    dbg!(&got.block);
    assert_eq!(got.block.transactions().len(), 1);
    assert_eq!(got.block.host_transactions().len(), 1);

    // Cleanup
    simulator_jh.abort();
}

async fn latest_sim_env(
    env_task: &EnvTask,
    host_provider: &HostProvider,
    ru_provider: &RuProvider,
) -> SimEnv {
    let host_previous =
        host_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;
    let ru_previous =
        ru_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;

    let host_env = env_task.construct_host_env(host_previous);
    let ru_env = env_task.construct_rollup_env(ru_previous);

    SimEnv { host: host_env, rollup: ru_env, span: tracing::Span::none() }
}

fn spawn_host_anvil() -> (AnvilInstance, HostProvider, PrivateKeySigner) {
    let anvil = Anvil::new().chain_id(constants().host_chain_id()).spawn();
    let key = anvil.keys()[0].clone();
    let signer = PrivateKeySigner::from_bytes(&B256::from_slice(&key.to_bytes())).unwrap();
    let wallet = anvil.wallet().expect("anvil wallet");
    let provider = ProviderBuilder::new_with_network()
        .disable_recommended_fillers()
        .filler(BlobGasFiller)
        .with_gas_estimation()
        .with_nonce_management(SimpleNonceManager::default())
        .fetch_chain_id()
        .wallet(wallet)
        .connect_http(anvil.endpoint_url());

    (anvil, provider, signer)
}

fn spawn_rollup_anvil() -> (AnvilInstance, RuProvider, Vec<PrivateKeySigner>) {
    let anvil = Anvil::new().chain_id(constants().ru_chain_id()).spawn();
    let signers: Vec<_> = anvil
        .keys()
        .iter()
        .take(2)
        .map(|key| PrivateKeySigner::from_bytes(&B256::from_slice(&key.to_bytes())).unwrap())
        .collect();
    assert!(signers.len() >= 2, "rollup anvil must provide at least two accounts");
    let provider = RootProvider::new_http(anvil.endpoint_url());

    (anvil, provider, signers)
}
