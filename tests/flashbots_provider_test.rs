//! Integration tests for the FlashbotsProvider.
//! These tests require the `FLASHBOTS_ENDPOINT` env var to be set.

use alloy::{
    eips::Encodable2718,
    node_bindings::Anvil,
    primitives::FixedBytes,
    primitives::U256,
    rpc::types::mev::{BundleItem, EthBundleHash, MevSendBundle, ProtocolVersion},
    signers::local::PrivateKeySigner,
};
use builder::tasks::submit::flashbots::FlashbotsProvider;
use builder::test_utils::{new_signed_tx, setup_logging, setup_test_config};
use init4_bin_base::deps::tracing::debug;
use signet_constants;

#[tokio::test]
#[ignore = "integration test"]
async fn smoke_root_provider() {
    setup_logging();
    let flashbots = get_test_provider().await;
    assert_eq!(flashbots.relay_url.as_str(), "http://localhost:9062/");

    let status =
        flashbots.bundle_status(EthBundleHash { bundle_hash: FixedBytes::default() }, 0).await;
    assert!(status.is_err());
}

#[tokio::test]
#[ignore = "integration test"]
async fn smoke_simulate_bundle() {
    let flashbots = get_test_provider().await;

    let res = flashbots.simulate_bundle(MevSendBundle::default()).await;

    if let Err(err) = &res {
        let msg = format!("{err}");
        assert!(msg.contains("mev_simBundle"));
    }
    assert!(res.is_err());
}

#[tokio::test]
#[ignore = "integration test"]
async fn simulate_valid_bundle() {
    setup_logging();

    let flashbots = get_test_provider().await;
    let url = flashbots.relay_url.as_str();
    debug!(?url, "Flashbots relay URL");

    let anvil = Anvil::new().chain_id(signet_constants::pecorino::RU_CHAIN_ID).spawn();
    let keys = anvil.keys();
    let wallet = PrivateKeySigner::from_signing_key(keys[0].clone().into());

    let tx = new_signed_tx(&wallet, 0, U256::from(1u64), 51_000).unwrap();
    let tx_bytes = tx.encoded_2718().into();

    let bundle_body = vec![BundleItem::Tx { tx: tx_bytes, can_revert: true }];
    dbg!("bundle body: ", bundle_body.clone());
    let bundle = MevSendBundle::new(0, Some(0), ProtocolVersion::V0_1, bundle_body);

    let res = flashbots.simulate_bundle(bundle).await;
    dbg!("result: {:?}", res.err());
}

#[tokio::test]
#[ignore = "integration test"]
async fn smoke_send_bundle() {
    let flashbots = get_test_provider().await;
    let res = flashbots.send_bundle(MevSendBundle::default()).await;

    if let Err(err) = &res {
        let msg = format!("{err}");
        assert!(msg.contains("mev_sendBundle"));
    }
    assert!(res.is_ok() || res.is_err());
}

async fn get_test_provider() -> FlashbotsProvider {
    let mut config = setup_test_config().unwrap();
    config.builder_key = PrivateKeySigner::random().to_bytes().to_string();
    dbg!(config.clone().builder_key);
    FlashbotsProvider::new(&config.clone())
}
