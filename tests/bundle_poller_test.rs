use builder::test_utils;
use eyre::Result;

#[ignore = "integration test"]
#[tokio::test]
async fn test_bundle_poller_roundtrip() -> Result<()> {
    let config = test_utils::setup_test_config().unwrap();
    let token = config.oauth_token();

    let mut bundle_poller = builder::tasks::cache::BundlePoller::new(&config, token);

    let _ = bundle_poller.check_bundle_cache().await?;

    Ok(())
}
