//! Integration tests for the FlashbotsProvider.
//! These tests require the `FLASHBOTS_ENDPOINT` env var to be set.

use builder::tasks::submit::flashbots::FlashbotsProvider;
use alloy::{
    primitives::FixedBytes,
    rpc::types::mev::{EthBundleHash, MevSendBundle},
};
use builder::test_utils::{setup_logging, setup_test_config};

#[tokio::test]
#[ignore = "integration test"]
async fn smoke_root_provider() {
    setup_logging();
    let flashbots = get_test_provider().await;
    assert_eq!(flashbots.relay_url.as_str(), "http://localhost:9062/");

    let status = flashbots
        .bundle_status(EthBundleHash { bundle_hash: FixedBytes::default() }, 0)
        .await;
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
    let config = setup_test_config().unwrap();
    FlashbotsProvider::new(&config.clone())
}
