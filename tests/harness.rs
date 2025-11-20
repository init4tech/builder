//! Test harness for end-to-end simulation flow.
//!
//! This harness wires a [`Simulator`] up to a manual [`SimEnv`] watch channel
//! and a submit channel so tests can manually control the simulation environment,
//! ticking along new blocks and asserting on the state of the simulator's results.
//!
//! It also manages two Anvil instances to simulate the rollup and host chains,
//! and provides utility functions for advancing blocks and adding transactions to
//! the simulator's mempool.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy::{
    consensus::Header,
    eips::{BlockId, eip1559::BaseFeeParams},
    network::{Ethereum, EthereumWallet},
    node_bindings::Anvil,
    primitives::{B256, U256},
    providers::{
        Provider, ProviderBuilder, RootProvider,
        ext::AnvilApi,
        fillers::{BlobGasFiller, SimpleNonceManager},
        layers::AnvilProvider,
    },
    signers::local::PrivateKeySigner,
};
use builder::{
    config::{BuilderConfig, HostProvider},
    tasks::{
        block::sim::{SimResult, Simulator},
        env::{Environment, SimEnv},
    },
    test_utils::{new_signed_tx_with_max_fee, setup_logging, setup_test_config},
};
use init4_bin_base::utils::calc::SlotCalculator;
use signet_sim::SimCache;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use trevm::revm::{context::BlockEnv, context_interface::block::BlobExcessGasAndPrice};

// Default test slot duration (seconds)
const DEFAULT_SLOT_DURATION: u64 = 5; // seconds
const TEST_TX_MAX_FEE: u128 = 1_000_000_000_000;

pub struct SimulatorTask {
    sim_env_tx: watch::Sender<Option<SimEnv>>,
    sim_env_rx: watch::Receiver<Option<SimEnv>>,
    sim_cache: SimCache,
}

pub struct TestHarness {
    /// Builder configuration for the Harness
    pub config: BuilderConfig,
    /// Anvil provider made from the Rollup Anvil instance
    pub rollup: RollupAnvilProvider,
    /// Anvil provider made from the Host Anvil instance
    pub host: HostAnvilProvider,
    /// The Simulator task that is assembled each tick.
    pub simulator: SimulatorTask,
    /// Transaction plumbing - Submit
    pub submit_tx: mpsc::UnboundedSender<SimResult>,
    /// Transaction plumbing - Receive
    pub submit_rx: mpsc::UnboundedReceiver<SimResult>,
    /// Keeps the simulator task alive for the duration of the harness so the
    /// background task isn't aborted when `start()` returns.
    pub simulator_handle: Option<JoinHandle<()>>,
}

type HostAnvilProvider = AnvilProvider<RootProvider<Ethereum>>;
type RollupAnvilProvider = AnvilProvider<RootProvider<Ethereum>>;

type RollupHeader = Header;
type HostHeader = Header;
type Headers = (RollupHeader, HostHeader);

impl TestHarness {
    /// Create a new harness with a fresh Anvil rollup chain and default test config.
    pub async fn new() -> eyre::Result<Self> {
        setup_logging();
        let mut config = setup_test_config()?;
        configure_slot_timing(&mut config)?;

        // Spawn host and rollup anvil chains (providers + keeping anvil alive)
        let host_anvil_provider = spawn_chain(signet_constants::pecorino::HOST_CHAIN_ID)?;
        let rollup_anvil_provider = spawn_chain(signet_constants::pecorino::RU_CHAIN_ID)?;

        // Create a new sim cache.
        let sim_cache = SimCache::new();

        // Plumb the sim environment and submit channels
        let (sim_env_tx, sim_env_rx) = watch::channel::<Option<SimEnv>>(None);
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<SimResult>();

        Ok(Self {
            config,
            rollup: rollup_anvil_provider,
            host: host_anvil_provider,
            simulator: SimulatorTask { sim_env_tx, sim_env_rx, sim_cache },
            submit_tx,
            submit_rx,
            simulator_handle: None,
        })
    }

    /// Add a signed transaction from a provided signer to the sim cache.
    pub fn add_tx(&self, signer: &PrivateKeySigner, nonce: u64, value: U256, mpfpg: u128) {
        let tx = new_signed_tx_with_max_fee(signer, nonce, value, mpfpg, TEST_TX_MAX_FEE)
            .expect("tx signing");
        // group index 0 for simplicity in tests
        self.simulator.sim_cache.add_tx(tx, 0);
    }

    /// Mine additional blocks on the underlying Anvil instance and update the sim_env with the latest headers.
    pub async fn mine_blocks(&self, count: u64) -> eyre::Result<()> {
        if count == 0 {
            return Ok(());
        }

        self.host.anvil_mine(Some(count), Some(1)).await?;
        self.rollup.anvil_mine(Some(count), Some(1)).await?;

        let (ru, host) = self.get_headers().await?;
        self.tick_from_headers(ru, host).await;

        Ok(())
    }

    /// Starts the simulator task.
    pub async fn start(&mut self) {
        if self.simulator_handle.is_some() {
            tracing::warn!("TestHarness simulator already running");
            return;
        }
        tracing::debug!("TestHarness starting simulator task");

        // Spawn the simulator background task
        let cache = self.simulator.sim_cache.clone();

        // Rollup provider
        let ru_provider = RootProvider::<Ethereum>::new_http(self.rollup.anvil().endpoint_url());

        // Host provider
        let host_provider = self.host_provider().await;

        // Wire up the simulator with the providers and submit channels
        let submit_tx = self.submit_tx.clone();
        let simulator = Simulator::new(
            &self.config,
            host_provider,
            ru_provider,
            self.simulator.sim_env_rx.clone(),
        );

        // Keep the JoinHandle on the harness so the task isn't aborted when
        // this function returns.
        let jh = simulator.spawn_simulator_task(cache, submit_tx);
        self.simulator_handle = Some(jh);
        tracing::debug!("TestHarness spawned simulator task");
    }

    /// Returns the latest rollup and host headers.
    pub async fn get_headers(&self) -> eyre::Result<Headers> {
        let ru_block = self.rollup.get_block(BlockId::latest()).await?;
        let ru_header = ru_block.expect("rollup latest exists").header.inner;

        let host_block = self.host.get_block(BlockId::latest()).await?;
        let host_header = host_block.expect("host latest exists").header.inner;

        tracing::debug!(
            rollup_header_number = ru_header.number,
            rollup_header_gas_limit = ru_header.gas_limit
        );

        let headers = (ru_header, host_header);
        Ok(headers)
    }

    /// Tick a new `SimEnv` computed from the current latest rollup and host headers.
    pub async fn tick_from_headers(&self, prev_ru_header: Header, prev_host_header: Header) {
        // Set new simulation deadline and target block number from previous header
        let target_ru_block_number = prev_ru_header.number + 1;
        let deadline = prev_ru_header.timestamp + self.config.slot_calculator.slot_duration();

        let span = tracing::info_span!(
            "TestHarness::tick",
            target_ru_block_number = target_ru_block_number,
            deadline = deadline,
            prev_ru_number = prev_ru_header.number,
            prev_host_number = prev_host_header.number
        );

        let host_env = build_host_environment(&self.config, prev_host_header);
        let rollup_env = build_rollup_environment(&self.config, prev_ru_header);

        self.simulator
            .sim_env_tx
            .send(Some(SimEnv { host: host_env, rollup: rollup_env, span }))
            .expect("send sim_env environment");
    }

    /// Receive the next SimResult with a timeout.
    pub async fn recv_result(&mut self, timeout: Duration) -> Option<SimResult> {
        tracing::debug!(?timeout, "TestHarness waiting for sim result");
        let res = tokio::time::timeout(timeout, self.submit_rx.recv()).await.ok().flatten();
        tracing::debug!(received = res.is_some(), "TestHarness recv_result returning");
        res
    }

    /// Stop the background simulator task if running.
    pub async fn stop(&mut self) {
        if let Some(handle) = self.simulator_handle.take() {
            // Abort and give it a short period to finish cleanup.
            handle.abort();
            let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;
        }
    }

    /// Returns a host provider configured with the builder's wallet and blob gas params
    async fn host_provider(&self) -> HostProvider {
        let wallet = EthereumWallet::from(
            self.config.connect_builder_signer().await.expect("builder signer"),
        );

        let host_provider_inner =
            RootProvider::<Ethereum>::new_http(self.host.anvil().endpoint_url());

        ProviderBuilder::new_with_network()
            .disable_recommended_fillers()
            .filler(BlobGasFiller)
            .with_gas_estimation()
            .with_nonce_management(SimpleNonceManager::default())
            .fetch_chain_id()
            .wallet(wallet)
            .connect_provider(host_provider_inner)
    }
}

// This function sets the slot timing to start now with a 10 second slot duration for tests.
fn configure_slot_timing(config: &mut BuilderConfig) -> Result<(), eyre::Error> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    config.slot_calculator = SlotCalculator::new(now, 0, DEFAULT_SLOT_DURATION);
    Ok(())
}

/// Builds a host environment from the given prev_header for the _next_ block.
fn build_host_environment(config: &BuilderConfig, prev_header: Header) -> Environment {
    let block_env = BlockEnv {
        number: U256::from(prev_header.number + 1),
        beneficiary: config.builder_rewards_address,
        timestamp: U256::from(prev_header.timestamp + config.slot_calculator.slot_duration()),
        gas_limit: config.max_host_gas(prev_header.gas_limit),
        basefee: prev_header
            .next_block_base_fee(BaseFeeParams::ethereum())
            .expect("signet has no non-1559 headers"),
        difficulty: U256::ZERO,
        prevrandao: Some(B256::random()),
        blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
            excess_blob_gas: 0,
            blob_gasprice: 0,
        }),
    };

    Environment::new(block_env, prev_header)
}

/// Builds a rollup environment from the given prev_header for the _next_ block.
fn build_rollup_environment(config: &BuilderConfig, prev_header: Header) -> Environment {
    let block_env = BlockEnv {
        number: U256::from(prev_header.number + 1),
        beneficiary: config.builder_rewards_address,
        timestamp: U256::from(prev_header.timestamp + config.slot_calculator.slot_duration()),
        gas_limit: config.rollup_block_gas_limit,
        basefee: prev_header
            .next_block_base_fee(BaseFeeParams::ethereum())
            .expect("signet has no non-1559 headers"),
        difficulty: U256::ZERO,
        prevrandao: Some(B256::random()),
        blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
            excess_blob_gas: 0,
            blob_gasprice: 0,
        }),
    };

    Environment::new(block_env, prev_header)
}

// Spawn an Anvil instance and return its provider and an AnvilProvider wrapper that
// keeps the Anvil process alive for the lifetime of the provider.
fn spawn_chain(chain_id: u64) -> eyre::Result<AnvilProvider<RootProvider<Ethereum>>> {
    let anvil = Anvil::new().chain_id(chain_id).spawn();
    let provider = RootProvider::<Ethereum>::new_http(anvil.endpoint_url().clone());
    let anvil_provider = AnvilProvider::new(provider.clone(), Arc::new(anvil));
    Ok(anvil_provider)
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        if let Some(handle) = self.simulator_handle.take() {
            handle.abort();
        }
    }
}
