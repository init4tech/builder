use std::str::FromStr;
use std::sync::Arc;

use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::rpc::types::mev::EthSendBundle;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use builder::config::BuilderConfig;
use builder::tasks::bundler::Bundle;
use builder::tasks::simulator::{EvmPool, Simulator};
use revm::primitives::{bytes, Address, TxKind, U256};
use tokio::select;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use trevm::{EvmFactory, NoopBlock, NoopCfg};
use zenith_types::ZenithEthBundle;

#[tokio::test]
async fn test_simulator_spawn() {
    // create a test config and provider
    let config = load_test_config();
    let ru_provider = config.connect_ru_provider().await.unwrap();

    let sim: Simulator<NoopCfg, NoopCfg, NoopBlock> = Simulator::new(ru_provider, config).await.unwrap();

    // plumb the mocked simulator
    let (bundle_sender, inbound_bundles) = mpsc::unbounded_channel();
    let (_tx_sender, inbound_txs) = mpsc::unbounded_channel();
    let (submit_sender, mut submit_receiver) = mpsc::unbounded_channel();

    // spawn the simulator
    let simulator_handle: JoinHandle<()> =
        sim.spawn(inbound_bundles, inbound_txs, submit_sender).await;

    // create a random test transaction
    let wallet = PrivateKeySigner::random();
    let test_tx = new_test_tx(&wallet).unwrap();

    // create a bundle and send it into the simulator
    bundle_sender
        .send(Arc::new(Bundle {
            id: "bundle1".to_string(),
            bundle: ZenithEthBundle {
                bundle: EthSendBundle {
                    txs: vec![test_tx.encoded_2718().into()],
                    block_number: 1,
                    min_timestamp: None,
                    max_timestamp: None,
                    reverting_tx_hashes: vec![],
                    replacement_uuid: None,
                },
                host_fills: None,
            },
        }))
        .unwrap();

    // Sleep for a short duration to allow the simulator to process the bundle
    let deadline = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(deadline);

    select! {
        _ = &mut deadline => {
            panic!("Simulator did not submit a block in time");
        }
        block = submit_receiver.recv() => {
            if let Some(block) = block {
                println!("receives test block {:?}", block);
                assert_eq!(block.len(), 2);
            } else {
                panic!("Simulator did not submit a block");
            }
        }
    }

    // Clean up
    simulator_handle.abort();
}

// Returns a new signed test transaction with default values
fn new_test_tx(wallet: &PrivateKeySigner) -> eyre::Result<TxEnvelope> {
    let tx = TxEip1559 {
        chain_id: 17001,
        nonce: 1,
        gas_limit: 50000,
        to: TxKind::Call(Address::from_str("0x0000000000000000000000000000000000000000").unwrap()),
        value: U256::from(1_f64),
        input: bytes!(""),
        ..Default::default()
    };
    let signature = wallet.sign_hash_sync(&tx.signature_hash())?;
    Ok(TxEnvelope::Eip1559(tx.into_signed(signature)))
}

/// Load up a config for test purposes
fn load_test_config() -> BuilderConfig {
    BuilderConfig {
        host_chain_id: 1,
        ru_chain_id: 2,
        host_rpc_url: "http://localhost:8545".into(), // TODO link this to a local anvil?
        ru_rpc_url: "http://localhost:8546".into(),   // TODO link this to a local signet-node
        tx_broadcast_urls: vec!["http://localhost:8547".into()],
        zenith_address: Address::from_str("0x0000000000000000000000000000000000000000").unwrap(),
        builder_helper_address: Address::from_str("0x0000000000000000000000000000000000000000")
            .unwrap(),
        quincey_url: "http://localhost:8548".into(),
        builder_port: 8080,
        sequencer_key: Some("test_sequencer_key".to_string()),
        builder_key: "test_builder_key".to_string(),
        block_confirmation_buffer: 10,
        chain_offset: 0,
        target_slot_time: 6,
        builder_rewards_address: Address::from_str("0x0000000000000000000000000000000000000000")
            .unwrap(),
        rollup_block_gas_limit: 1000000,
        tx_pool_url: "http://localhost:8549".into(),
        tx_pool_cache_duration: 60,
        oauth_client_id: "test_client_id".to_string(),
        oauth_client_secret: "test_client_secret".to_string(),
        oauth_authenticate_url: "http://localhost:8550/auth".to_string(),
        oauth_token_url: "http://localhost:8550/token".to_string(),
        oauth_token_refresh_interval: 3600,
    }
}
