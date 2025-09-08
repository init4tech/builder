use alloy::{
    consensus::Transaction,
    eips::Encodable2718,
    primitives::U256,
    providers::Provider,
    rpc::types::mev::{BundleItem, MevSendBundle, ProtocolVersion},
    signers::local::PrivateKeySigner,
};
use builder::{
    tasks::submit::flashbots::Flashbots,
    test_utils::{new_signed_tx, setup_logging, setup_sepolia_config},
};
use std::str::FromStr;

#[tokio::test]
#[ignore = "integration test"]
async fn test_simulate_valid_bundle_sepolia() {
    setup_logging();

    let flashbots = get_test_provider().await;

    let signer = flashbots.config.builder_key.clone();
    let signer = PrivateKeySigner::from_str(&signer).unwrap();
    dbg!("using builder key", signer.address());

    let tx = new_signed_tx(&signer, 0, U256::from(1u64), 51_000).unwrap();
    let tx_bytes = tx.encoded_2718().into();

    let host_provider = flashbots.config.connect_host_provider().await.unwrap();
    let latest_block = host_provider
        .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest)
        .await
        .unwrap()
        .unwrap()
        .number();
    dbg!("latest block number", latest_block);
    dbg!(tx.chain_id());

    let bundle_body = vec![BundleItem::Tx { tx: tx_bytes, can_revert: true }];
    dbg!("submitting bundle with 1 tx", &bundle_body);
    let bundle = MevSendBundle::new(latest_block, Some(0), ProtocolVersion::V0_1, bundle_body);

    flashbots.simulate_bundle(bundle).await.expect("failed to simulate bundle");
}

async fn get_test_provider() -> Flashbots {
    let config = setup_sepolia_config().unwrap();
    Flashbots::new(&config.clone()).await
}
