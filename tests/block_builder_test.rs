#[cfg(test)]
mod tests {
    use alloy::{
        consensus::{SignableTransaction, TxEip1559, TxEnvelope},
        node_bindings::Anvil,
        primitives::{Address, TxKind, U256, bytes},
        providers::ProviderBuilder,
        signers::{SignerSync, local::PrivateKeySigner},
    };
    use builder::{
        config::BuilderConfig,
        tasks::{block::BlockBuilder, bundler::Bundle, oauth::Authenticator},
    };
    use eyre::Result;
    use signet_sim::BuiltBlock;
    use std::str::FromStr;
    use tokio::sync::mpsc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_spawn() {
        // Make a test config
        let config = setup_test_config();
        let constants = config.load_pecorino_constants();

        // Create an authenticator for bundle integration testing
        let authenticator = Authenticator::new(&config);

        // Create an anvil instance for testing
        let anvil_instance = Anvil::new().chain_id(14174).spawn();
        let keys = anvil_instance.keys();
        let test_key = PrivateKeySigner::from_signing_key(keys[0].clone().into());

        // Create a rollup provider
        let ru_provider = ProviderBuilder::new().on_http(anvil_instance.endpoint_url());

        // Create a block builder
        let block_builder = BlockBuilder::new(&config, authenticator, ru_provider.clone());

        // Set up channels for tx, bundle, and outbound built blocks
        let (tx_sender, tx_receiver) = mpsc::unbounded_channel::<TxEnvelope>();
        let (_bundle_sender, bundle_receiver) = mpsc::unbounded_channel::<Bundle>();
        let (outbound_sender, mut outbound_receiver) = mpsc::unbounded_channel::<BuiltBlock>();

        // Spawn the block builder task
        let _jh = block_builder.spawn(
            constants,
            ru_provider,
            tx_receiver,
            bundle_receiver,
            outbound_sender,
        );

        // send two transactions
        let tx_1 = new_test_tx(&test_key, 1, U256::from(1_f64)).unwrap();
        tx_sender.send(tx_1).unwrap();

        let tx_2 = new_test_tx(&test_key, 2, U256::from(2_f64)).unwrap();
        tx_sender.send(tx_2).unwrap();

        let block = outbound_receiver.recv().await;

        println!("block: {:?}", block);
        assert!(block.is_some());
        assert!(block.unwrap().tx_count() == 2);
    }

    fn setup_test_config() -> BuilderConfig {
        let config = BuilderConfig {
            host_chain_id: 17000,
            ru_chain_id: 17001,
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
            rollup_block_gas_limit: 100_000,
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
    fn new_test_tx(wallet: &PrivateKeySigner, nonce: u64, value: U256) -> Result<TxEnvelope> {
        let tx = TxEip1559 {
            chain_id: 17001,
            nonce,
            gas_limit: 50000,
            to: TxKind::Call(
                Address::from_str("0x0000000000000000000000000000000000000000").unwrap(),
            ),
            value,
            input: bytes!(""),
            ..Default::default()
        };
        let signature = wallet.sign_hash_sync(&tx.signature_hash())?;
        Ok(TxEnvelope::Eip1559(tx.into_signed(signature)))
    }
}
