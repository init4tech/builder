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
    eips::BlockId,
    network::Ethereum,
    node_bindings::Anvil,
    primitives::U256,
    providers::{Provider, RootProvider, ext::AnvilApi, layers::AnvilProvider},
    signers::local::PrivateKeySigner,
};
use builder::{
    config::BuilderConfig,
    tasks::{
        block::sim::{SimResult, Simulator},
        env::SimEnv,
    },
    test_utils::{new_signed_tx, setup_logging, setup_test_config, test_block_env},
};
use init4_bin_base::utils::calc::SlotCalculator;
use signet_sim::SimCache;
use signet_types::constants::SignetSystemConstants;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

// 5 seconds of slot time means 3 seconds of simulation time.
const DEFAULT_SLOT_DURATION: u64 = 5; // seconds

pub struct SimulatorTask {
    sim_env_tx: watch::Sender<Option<SimEnv>>,
    sim_env_rx: watch::Receiver<Option<SimEnv>>,
    sim_cache: SimCache,
}

pub struct TestHarness {
    /// Builder configuration for the Harness
    pub config: BuilderConfig,
    /// System constants for the Harness
    pub constants: SignetSystemConstants,
    /// Anvil provider made from the Rollup Anvil instance 
    pub rollup: RollupAnvilProvider,
    /// Anvilk provider made from the Host Anvil instance
    pub host: HostAnvilProvider,
    /// The Simulator task that is assembled each tick.
    pub simulator: SimulatorTask,
    /// Transaction plumbing - Submit 
    submit_tx: mpsc::UnboundedSender<SimResult>,
    /// Transaction plumbing - Receive
    submit_rx: mpsc::UnboundedReceiver<SimResult>,
    /// Keeps the simulator task alive for the duration of the harness so the
    /// background task isn't aborted when `start()` returns.
    simulator_handle: Option<JoinHandle<()>>,
}

type HostAnvilProvider = AnvilProvider<RootProvider<Ethereum>>;
type RollupAnvilProvider = AnvilProvider<RootProvider<Ethereum>>;

impl TestHarness {
    /// Create a new harness with a fresh Anvil rollup chain and default test config.
    pub async fn new() -> eyre::Result<Self> {
        setup_logging();

        // Make a new test config and set its slot timing
        let mut config = setup_test_config()?;
        
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
            constants: SignetSystemConstants::pecorino(),
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
        let tx = new_signed_tx(signer, nonce, value, mpfpg).expect("tx signing");
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
    pub fn start(&mut self) {
        if self.simulator_handle.is_some() {
            tracing::warn!("TestHarness simulator already running");
            return;
        }

        tracing::debug!("TestHarness starting simulator task");
        // Spawn the simulator background task
        let cache = self.simulator.sim_cache.clone();
        let constants = self.constants.clone();

        // Create a rollup provider from the rollup anvil
        let ru_provider = RootProvider::<Ethereum>::new_http(self.rollup.anvil().endpoint_url());

        // Wire up the simulator with that provider and submit channels
        let submit_tx = self.submit_tx.clone();
        let simulator =
            Simulator::new(&self.config, ru_provider, self.simulator.sim_env_rx.clone());

        // Keep the JoinHandle on the harness so the task isn't aborted when
        // this function returns.
        let jh = simulator.spawn_simulator_task(constants, cache, submit_tx);
        self.simulator_handle = Some(jh);
        tracing::debug!("TestHarness spawned simulator task");
    }

    /// Returns the latest rollup and host headers.
    pub async fn get_headers(&self) -> eyre::Result<(Header, Header)> {
        let ru_block = self.rollup.get_block(BlockId::latest()).await?;
        let ru_header = ru_block.expect("rollup latest exists").header.inner;

        let host_block = self.host.get_block(BlockId::latest()).await?;
        let host_header = host_block.expect("host latest exists").header.inner;

        Ok((ru_header, host_header))
    }

    /// Tick a new SimEnv computed from the current host latest header.
    pub async fn tick_from_host(&self) {
        let header = self
            .host
            .get_block(BlockId::latest())
            .await
            .expect("host latest block")
            .expect("host latest exists")
            .header
            .inner;

        let target_block_number = header.number + 1;
        let deadline = header.timestamp + DEFAULT_SLOT_DURATION;
        let block_env = test_block_env(self.config.clone(), target_block_number, 7, deadline);

        assert!(deadline > header.timestamp);
        assert!(header.number < target_block_number);

        let span =
            tracing::info_span!("TestHarness::tick_from_host", target_block_number, deadline);
        let sim_env = SimEnv { block_env, prev_header: header.clone(), prev_host: header, span };

        let _ = self.simulator.sim_env_tx.send(Some(sim_env));
    }

    /// Tick a new `SimEnv` computed from the current latest rollup and host headers.
    pub async fn tick_from_headers(&self, prev_ru_header: Header, prev_host_header: Header) {
        let target_ru_block_number = prev_ru_header.number + 1;

        // Set new simulation deadline from previous header
        let deadline = prev_ru_header.timestamp + self.config.slot_calculator.slot_duration();

        // Make a new block env from the previous rollup header and our new simulation deadline.
        let block_env = test_block_env(self.config.clone(), target_ru_block_number, 7, deadline);

        let span = tracing::info_span!("TestHarness::tick", target_ru_block_number, deadline);

        // Make a new SimEnv and send it to the simulator task.
        let sim_env = SimEnv {
            block_env,
            prev_header: prev_ru_header.clone(),
            prev_host: prev_host_header,
            span,
        };

        let _ = self.simulator.sim_env_tx.send(Some(sim_env));
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
}

// This function sets the slot timing to start now with a 10 second slot duration for tests.
fn configure_slot_timing(config: &mut BuilderConfig) -> Result<(), eyre::Error> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    config.slot_calculator = SlotCalculator::new(now, 0, DEFAULT_SLOT_DURATION);
    Ok(())
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
