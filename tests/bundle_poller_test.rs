use builder::test_utils::{setup_logging, setup_test_config};
use eyre::Result;

#[ignore = "integration test"]
#[tokio::test]
async fn test_bundle_poller_roundtrip() -> Result<()> {
    setup_logging();
    setup_test_config();

    let bundle_poller = builder::tasks::cache::BundlePoller::new();

    let _ = bundle_poller.check_bundle_cache().await?;

    Ok(())
}
