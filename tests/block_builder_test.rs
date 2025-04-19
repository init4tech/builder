#[cfg(test)]
mod tests {
    use alloy::{
        node_bindings::Anvil, primitives::U256, providers::ProviderBuilder,
        signers::local::PrivateKeySigner,
    };
    use builder::{
        tasks::block::{BlockBuilder, PECORINO_CHAIN_ID},
        test_utils::{new_signed_tx, setup_logging, setup_test_config},
    };

    use signet_sim::{SimCache, SimItem};
    use signet_types::SlotCalculator;
    use std::{
        sync::Arc,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use tokio::{sync::mpsc::unbounded_channel, time::timeout};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_handle_build() {
        setup_logging();

        // Make a test config
        let config = setup_test_config().unwrap();
        let constants = config.load_pecorino_constants();

        // Create an anvil instance for testing
        let anvil_instance = Anvil::new().chain_id(PECORINO_CHAIN_ID).spawn();

        // Create a wallet
        let keys = anvil_instance.keys();
        let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
        let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

        // Create a rollup provider
        let ru_provider = ProviderBuilder::new().on_http(anvil_instance.endpoint_url());

        // Create a block builder with a slot calculator for testing
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Clock may have gone backwards")
            .as_secs();
        dbg!(now);
        let slot_calculator = SlotCalculator::new(now, 0, 12);
        let block_builder = BlockBuilder::new(&config, ru_provider.clone(), slot_calculator);

        // Setup a sim cache
        let sim_items = SimCache::new();

        // Add two transactions from two senders to the sim cache
        let tx_1 = new_signed_tx(&test_key_0, 0, U256::from(1_f64), 11_000).unwrap();
        sim_items.add_item(SimItem::Tx(tx_1));

        let tx_2 = new_signed_tx(&test_key_1, 0, U256::from(2_f64), 10_000).unwrap();
        sim_items.add_item(SimItem::Tx(tx_2));

        let finish_by = Instant::now() + Duration::from_secs(2);

        // Spawn the block builder task
        let got = block_builder.handle_build(constants, sim_items, finish_by).await;

        // Assert on the built block
        assert!(got.is_ok());
        assert!(got.unwrap().tx_count() == 2);
    }

    /// Tests the full block builder loop
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_spawn() {
        setup_logging();

        // Make a test config
        let config = setup_test_config().unwrap();
        let constants = config.load_pecorino_constants();

        // Create an anvil instance for testing
        let anvil_instance = Anvil::new().chain_id(PECORINO_CHAIN_ID).spawn();

        // Create a wallet
        let keys = anvil_instance.keys();
        let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
        let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

        // Plumb inputs for the test setup
        let (tx_sender, tx_receiver) = unbounded_channel();
        let (_, bundle_receiver) = unbounded_channel();
        let (block_sender, mut block_receiver) = unbounded_channel();

        // Create a rollup provider
        let ru_provider = ProviderBuilder::new().on_http(anvil_instance.endpoint_url());

        // Create a builder with a test slot calculator
        let slot_calculator = SlotCalculator::new(
            config.start_timestamp,
            config.chain_offset,
            config.target_slot_time,
        );
        let sim_cache = SimCache::new();
        let builder = Arc::new(BlockBuilder::new(&config, ru_provider.clone(), slot_calculator));

        // Create a sim cache and start filling it with items
        let _ =
            builder.clone().spawn_cache_handler(tx_receiver, bundle_receiver, sim_cache.clone());

        // Finally, Kick off the block builder task.
        let _ = builder.clone().spawn_builder_task(constants, sim_cache.clone(), block_sender);

        let tx_1 = new_signed_tx(&test_key_0, 0, U256::from(1_f64), 11_000).unwrap();
        let tx_2 = new_signed_tx(&test_key_1, 0, U256::from(2_f64), 10_000).unwrap();
        tx_sender.send(tx_1).unwrap();
        tx_sender.send(tx_2).unwrap();

        // Wait for a block with timeout
        let result = timeout(Duration::from_secs(5), block_receiver.recv()).await;
        assert!(result.is_ok(), "Did not receive block within 5 seconds");
        let block = result.unwrap();
        dbg!(&block);
        assert!(block.is_some(), "Block channel closed without receiving a block");
        assert!(block.unwrap().tx_count() == 2); // TODO: Why is this failing? I'm seeing EVM errors but haven't tracked them down yet. 
    }
}
