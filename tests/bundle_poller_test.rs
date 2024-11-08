mod tests {
    use alloy_primitives::Address;
    use builder::{
        config::BuilderConfig,
        tasks::{block::BlockBuilder, oauth::Authenticator},
    };
    use eyre::Result;

    #[ignore = "integration test"]
    #[tokio::test]
    async fn test_bundle_poller_roundtrip() -> Result<()> {
        let (_, config) = setup_test_builder().await.unwrap();
        let auth = Authenticator::new(&config).await?;
        let mut bundle_poller = builder::tasks::bundler::BundlePoller::new(&config, auth).await;

        let got = bundle_poller.check_bundle_cache().await?;
        dbg!(got);

        Ok(())
    }

    // TODO: Deduplicate this with the same function in tx_poller_test.rs
    async fn setup_test_builder() -> Result<(BlockBuilder, BuilderConfig)> {
        let config = BuilderConfig {
            host_chain_id: 17000,
            ru_chain_id: 17001,
            host_rpc_url: "http://rpc.holesky.signet.sh".into(),
            zenith_address: Address::default(),
            quincey_url: "http://localhost:8080".into(),
            builder_port: 8080,
            sequencer_key: None,
            builder_key: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            incoming_transactions_buffer: 1,
            block_confirmation_buffer: 1,
            builder_rewards_address: Address::default(),
            rollup_block_gas_limit: 100_000,
            tx_pool_url: "http://localhost:9000/".into(),
            // tx_pool_url: "https://transactions.holesky.signet.sh".into(),
            tx_pool_cache_duration: 5,
            tx_pool_poll_interval: 5,
            oauth_client_id: "some_client_id".into(),
            oauth_client_secret: "some_client_secret".into(),
            oauth_authenticate_url: "http://localhost:8080".into(),
            oauth_token_url: "http://localhost:8080".into(),
            oauth_audience: "https://transactions.holesky.signet.sh".into(),
            tx_broadcast_urls: vec!["http://localhost:9000".into()],
        };
        Ok((BlockBuilder::new(&config), config))
    }
}
