//! Tests for the block building task.
#[cfg(test)]
mod tests {
    use alloy::{
        network::Ethereum,
        node_bindings::Anvil,
        primitives::U256,
        providers::{Provider, RootProvider},
        signers::local::PrivateKeySigner,
    };
    use builder::{
        tasks::block::sim::Simulator,
        test_utils::{new_signed_tx, setup_logging, setup_test_config, test_block_env},
    };
    use init4_bin_base::utils::calc::SlotCalculator;
    use signet_sim::{SimCache, SimItem};
    use signet_types::constants::SignetSystemConstants;
    use std::{
        sync::Arc,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use tokio::{sync::mpsc::unbounded_channel, time::timeout};

    /// Tests the `handle_build` method of the `Simulator`.
    ///
    /// This test sets up a simulated environment using Anvil, creates a block builder,
    /// and verifies that the block builder can successfully build a block containing
    /// transactions from multiple senders.
    #[ignore = "integration test"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_handle_build() {
        setup_logging();

        // Make a test config
        let config = setup_test_config().unwrap();
        let constants = SignetSystemConstants::pecorino();

        // Create an anvil instance for testing
        let anvil_instance = Anvil::new().chain_id(signet_constants::pecorino::RU_CHAIN_ID).spawn();

        // Create a wallet
        let keys = anvil_instance.keys();
        let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
        let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

        // Create a rollup provider
        let ru_provider = RootProvider::<Ethereum>::new_http(anvil_instance.endpoint_url());

        // Create a block builder with a slot calculator for testing
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Clock may have gone backwards")
            .as_secs();

        let slot_calculator = SlotCalculator::new(now, 0, 12);
        let block_builder = Simulator::new(&config, ru_provider.clone(), slot_calculator);

        // Setup a sim cache
        let sim_items = SimCache::new();

        // Add two transactions from two senders to the sim cache
        let tx_1 = new_signed_tx(&test_key_0, 0, U256::from(1_f64), 11_000).unwrap();
        sim_items.add_item(SimItem::Tx(tx_1), 0);

        let tx_2 = new_signed_tx(&test_key_1, 0, U256::from(2_f64), 10_000).unwrap();
        sim_items.add_item(SimItem::Tx(tx_2), 0);

        // Setup the block env
        let finish_by = Instant::now() + Duration::from_secs(2);
        let block_number = ru_provider.get_block_number().await.unwrap();
        let block_env = test_block_env(config, block_number, 7, finish_by);

        // Spawn the block builder task
        let got = block_builder.handle_build(constants, sim_items, finish_by, block_env).await;

        // Assert on the built block
        assert!(got.is_ok());
        assert!(got.unwrap().tx_count() == 2);
    }

    /// Tests the full block builder loop, including transaction ingestion and block simulation.
    ///
    /// This test sets up a simulated environment using Anvil, creates a block builder,
    /// and verifies that the builder can process incoming transactions and produce a block
    /// within a specified timeout.
    #[ignore = "integration test"]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_spawn() {
        setup_logging();

        // Make a test config
        let config = setup_test_config().unwrap();
        let constants = SignetSystemConstants::pecorino();

        // Create an anvil instance for testing
        let anvil_instance = Anvil::new().chain_id(signet_constants::pecorino::RU_CHAIN_ID).spawn();

        // Create a wallet
        let keys = anvil_instance.keys();
        let test_key_0 = PrivateKeySigner::from_signing_key(keys[0].clone().into());
        let test_key_1 = PrivateKeySigner::from_signing_key(keys[1].clone().into());

        // Plumb inputs for the test setup
        let (tx_sender, tx_receiver) = unbounded_channel();
        let (_, bundle_receiver) = unbounded_channel();
        let (block_sender, mut block_receiver) = unbounded_channel();

        // Create a rollup provider
        let ru_provider = RootProvider::<Ethereum>::new_http(anvil_instance.endpoint_url());

        let sim = Arc::new(Simulator::new(&config, ru_provider.clone(), config.slot_calculator));

        // Create a shared sim cache
        let sim_cache = SimCache::new();

        // Create a sim cache and start filling it with items
        sim.clone().spawn_cache_tasks(tx_receiver, bundle_receiver, sim_cache.clone());

        // Finally, Kick off the block builder task.
        sim.clone().spawn_simulator_task(constants, sim_cache.clone(), block_sender);

        // Feed in transactions to the tx_sender and wait for the block to be simulated
        let tx_1 = new_signed_tx(&test_key_0, 0, U256::from(1_f64), 11_000).unwrap();
        let tx_2 = new_signed_tx(&test_key_1, 0, U256::from(2_f64), 10_000).unwrap();
        tx_sender.send(tx_1).unwrap();
        tx_sender.send(tx_2).unwrap();

        // Wait for a block with timeout
        let result = timeout(Duration::from_secs(5), block_receiver.recv()).await;
        assert!(result.is_ok(), "Did not receive block within 5 seconds");

        // Assert on the block
        let block = result.unwrap();
        assert!(block.is_some(), "Block channel closed without receiving a block");
        assert!(block.unwrap().tx_count() == 2); // TODO: Why is this failing? I'm seeing EVM errors but haven't tracked them down yet.
    }
}
