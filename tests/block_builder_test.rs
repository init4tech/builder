//! Tests for the block building task.

use alloy::{
    consensus::BlockHeader,
    eips::{BlockId, Encodable2718},
    network::EthereumWallet,
    node_bindings::Anvil,
    primitives::{Address, B256, Bytes, U256},
    providers::{Provider, ProviderBuilder},
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
use trevm::revm::{context::BlockEnv, context_interface::block::BlobExcessGasAndPrice};

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

    let _config = setup_test_config();

    let host_anvil_instance = Anvil::new().fork("ws://host-rpc.pecorino.signet.sh").spawn();
    let host_provider =
        ProviderBuilder::new().connect_http(host_anvil_instance.endpoint().parse().unwrap());
    let host_chain_id = host_anvil_instance.chain_id();

    // TODO: Send the built block to Flashbots and then assert on what comes out of it - a MEV Send Bundle type
    // All of those values need to match up correctly
    // And the bundle is currently using the rollup block number, not the host block number, that's showing up in my
    // tests right now, and my setup logic is sound as far as I can tell.
    //
    // SOOOOOOO I think the issue is in the Flashbots task conversion logic or the simulation setup logic.
    let (flashbots_tx, flashbots_rx) = unbounded_channel();
    let flashbots_task =
        builder::tasks::submit::flashbots::FlashbotsTask::new(flashbots_tx).await.unwrap();

    let ru_provider = builder::config().connect_ru_provider().await.unwrap();
    let ru_chain_id = ru_provider.get_chain_id().await.expect("gets ru chain id");
    dbg!(&host_anvil_instance.endpoint());

    // let keys = host_anvil_instance.keys();

    let priv_key =
        std::env::var("SIGNET_WALLET_TEST_PK").expect("SIGNET_WALLET_TEST_PK env var to be set");

    let test_key = PrivateKeySigner::from_str(&priv_key).expect("valid private key");

    let host_test_key_0 =
        PrivateKeySigner::from_bytes(&test_key.clone().to_bytes()).expect("test key 0 to be set");
    let host_test_key_1 =
        PrivateKeySigner::from_bytes(&test_key.clone().to_bytes()).expect("test key 1 to be set");

    let host_header =
        host_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;

    let ru_header = ru_provider.get_block(BlockId::latest()).await.unwrap().unwrap().header.inner;

    let (sim_env_tx, sim_env_rx) = channel::<Option<SimEnv>>(None);
    let simulator_task = SimulatorTask::new(sim_env_rx).await.unwrap();

    let (submit_channel, submit_jh) = flashbots_task.spawn();

    let sim_items = SimCache::new();

    let simulator_task = simulator_task.spawn_simulator_task(sim_items, submit_channel);

    let host_block_env = {
        let number = host_header.number();
        let timestamp = host_header.timestamp;
        let config = setup_test_config();

        BlockEnv {
            number: U256::from(number),
            beneficiary: Address::repeat_byte(1),
            timestamp: U256::from(timestamp),
            gas_limit: config.rollup_block_gas_limit,
            basefee: 7,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::random()),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
                excess_blob_gas: 0,
                blob_gasprice: 0,
            }),
        }
    };

    let ru_block_env = {
        let number = ru_header.number();
        let timestamp = ru_header.timestamp;
        let config = setup_test_config();

        BlockEnv {
            number: U256::from(number),
            beneficiary: Address::repeat_byte(1),
            timestamp: U256::from(timestamp),
            gas_limit: config.rollup_block_gas_limit,
            basefee: 7,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::random()),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
                excess_blob_gas: 0,
                blob_gasprice: 0,
            }),
        }
    };

    let sim_env = SimEnv {
        host: Environment::new(host_block_env.clone(), host_header.clone()),
        rollup: Environment::new(ru_block_env.clone(), ru_header),
        span: tracing::Span::none(),
    };

    sim_env_tx.send(Some(sim_env.clone())).unwrap();

    let ru_tx = new_signed_tx(&host_test_key_1, ru_chain_id, 1, U256::from(1), 10_000).unwrap();
    let ru_txn_buf: Bytes = ru_tx.encoded_2718().into();

    let host_txn =
        new_signed_tx(&host_test_key_0, host_chain_id, 1, U256::from(1), 10_000).unwrap();
    let host_txn_buf: Bytes = host_txn.encoded_2718().into();

    // Target rollup block number
    let block_number = sim_env.rollup_block_number();

    assert!(block_number == host_header.number() + 1);
    dbg!("BUILDING FOR HOST BLOCK NUMBER: ", &block_number);

    let bundle = SignetEthBundle {
        bundle: EthSendBundle {
            replacement_uuid: Some("test-replacement-uuid".to_string()),
            txs: vec![ru_txn_buf],
            block_number,
            ..Default::default()
        },
        host_txs: vec![host_txn_buf],
    };

    sim_items.add_bundle(bundle, 7).expect("adds bundle to the sim cache");

    let finish_by = Instant::now() + Duration::from_secs(3);

    dbg!(built.clone());
    let got_block_number = built.block_number();
}
