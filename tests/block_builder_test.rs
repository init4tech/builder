#[cfg(test)]
mod tests {
    use alloy::{
        consensus::{SignableTransaction, TxEip1559, TxEnvelope},
        node_bindings::Anvil,
        primitives::{Address, TxKind, U256},
        providers::ProviderBuilder,
        signers::{SignerSync, local::PrivateKeySigner},
    };
    use builder::{
        config::BuilderConfig,
        tasks::{
            block::{BlockBuilder, PECORINO_CHAIN_ID},
            oauth::Authenticator,
        },
    };
    use eyre::Result;
    use signet_sim::{SimCache, SimItem};
    use std::str::FromStr;
    use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_spawn() {
        let filter = EnvFilter::from_default_env();
        let fmt = tracing_subscriber::fmt::layer().with_filter(filter);
        let registry = tracing_subscriber::registry().with(fmt);
        registry.try_init().unwrap();

        // Make a test config
        let config = setup_test_config();
        let constants = config.load_pecorino_constants();

        // Create an authenticator for bundle integration testing
        let authenticator = Authenticator::new(&config);

        // Create an anvil instance for testing
        let anvil_instance = Anvil::new().chain_id(PECORINO_CHAIN_ID).spawn();

        // Create a wallet
        let keys = anvil_instance.keys();
        let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
        let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

        // Create a rollup provider
        let ru_provider = ProviderBuilder::new().on_http(anvil_instance.endpoint_url());

        // Create a block builder
        let block_builder = BlockBuilder::new(&config, authenticator, ru_provider.clone());

        // Setup a sim cache
        let sim_items = SimCache::new();

        // Add two transactions from two senders to the sim cache
        let tx_1 = new_signed_tx(&test_key_0, 0, U256::from(1_f64), 11_000).unwrap();
        sim_items.add_item(SimItem::Tx(tx_1));

        let tx_2 = new_signed_tx(&test_key_1, 0, U256::from(2_f64), 10_000).unwrap();
        sim_items.add_item(SimItem::Tx(tx_2));

        // Spawn the block builder task
        let got = block_builder.handle_build(constants, ru_provider, sim_items).await;

        // Assert on the built block
        assert!(got.is_ok());
        assert!(got.unwrap().tx_count() == 2);
    }

    fn setup_test_config() -> BuilderConfig {
        let config = BuilderConfig {
            host_chain_id: 1,
            ru_chain_id: PECORINO_CHAIN_ID,
            host_rpc_url: "https://host-rpc.example.com".into(),
            ru_rpc_url: "https://rpc.pecorino.signet.sh".into(),
            zenith_address: Address::default(),
            quincey_url: "http://localhost:8080".into(),
            builder_port: 8080,
            sequencer_key: None,
            builder_key: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            block_confirmation_buffer: 1,
            chain_offset: 0,
            target_slot_time: 1,
            builder_rewards_address: Address::default(),
            rollup_block_gas_limit: 100_000_000,
            tx_pool_url: "http://localhost:9000/".into(),
            tx_pool_cache_duration: 5,
            oauth_client_id: "some_client_id".into(),
            oauth_client_secret: "some_client_secret".into(),
            oauth_authenticate_url: "http://localhost:9000".into(),
            oauth_token_url: "http://localhost:9000".into(),
            tx_broadcast_urls: vec!["http://localhost:9000".into()],
            oauth_token_refresh_interval: 300, // 5 minutes
            builder_helper_address: Address::default(),
            concurrency_limit: 1000,
        };
        config
    }

    // Returns a new signed test transaction with default values
    fn new_signed_tx(
        wallet: &PrivateKeySigner,
        nonce: u64,
        value: U256,
        mpfpg: u128,
    ) -> Result<TxEnvelope> {
        let tx = TxEip1559 {
            chain_id: PECORINO_CHAIN_ID,
            nonce,
            max_fee_per_gas: 50_000,
            max_priority_fee_per_gas: mpfpg,
            to: TxKind::Call(
                Address::from_str("0x0000000000000000000000000000000000000000").unwrap(),
            ),
            value,
            gas_limit: 50_000,
            ..Default::default()
        };
        let signature = wallet.sign_hash_sync(&tx.signature_hash())?;
        Ok(TxEnvelope::Eip1559(tx.into_signed(signature)))
    }
}
