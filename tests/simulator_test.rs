use std::str::FromStr;

use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::eips::eip2718::Encodable2718;
use alloy::rpc::types::mev::EthSendBundle;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use builder::config::BuilderConfig;
use builder::tasks::bundler::Bundle;
use builder::tasks::simulator::Simulator;
use revm::primitives::{bytes, Address, TxKind, U256};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zenith_types::ZenithEthBundle;

#[tokio::test]
async fn test_simulator_spawn() {
    // create a mocked simulator
    let config = BuilderConfig::load_from_env().unwrap();
    let ru_provider = config.connect_ru_provider().await.unwrap();
    let simulator = Simulator::new(ru_provider, config.clone()).await.unwrap();

    // plumb the mocked simulator
    let (bundle_sender, inbound_bundles) = mpsc::unbounded_channel();
    let (_tx_sender, inbound_txs) = mpsc::unbounded_channel();
    let (submit_sender, mut submit_receiver) = mpsc::unbounded_channel();

    // spawn the simulator
    let simulator_handle: JoinHandle<()> =
        simulator.spawn(inbound_bundles, inbound_txs, submit_sender).await;

    // create a random test transaction
    let wallet = PrivateKeySigner::random();
    let test_tx = new_test_tx(&wallet).unwrap();

    // create a bundle and send it into the simulator
    bundle_sender
        .send(Bundle {
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
        })
        .unwrap();

    // Check if the simulator submitted a block and that it has two transactions as it should
    if let Some(block) = submit_receiver.recv().await {
        assert_eq!(block.len(), 2);
    } else {
        panic!("Simulator did not submit a block");
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
