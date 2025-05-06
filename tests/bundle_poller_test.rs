mod tests {
    use builder::{tasks::oauth::Authenticator, test_utils};
    use eyre::Result;

    #[ignore = "integration test"]
    #[tokio::test]
    async fn test_bundle_poller_roundtrip() -> Result<()> {
        let config = test_utils::setup_test_config().unwrap();
        let auth = Authenticator::new(&config);

        let mut bundle_poller = builder::tasks::bundler::BundlePoller::new(&config, auth);

        let _ = bundle_poller.check_bundle_cache().await?;

        Ok(())
    }
}
