//! Test harness for end-to-end simulation flow.
//!
//! This harness wires a Simulator to a manual SimEnv watch channel and an
//! mpsc submit channel so tests can tick BlockEnv values and assert on
//! emitted SimResult blocks without involving submission logic.

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
use signet_sim::SimCache;
use signet_types::constants::SignetSystemConstants;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

const DEFAULT_BLOCK_TIME: u64 = 5; // seconds

pub struct HostChain {
    /// The provider for the host chain.
    pub provider: RootProvider<Ethereum>,
    /// The Anvil provider for the host chain, used to control mining in tests.
    pub anvil: AnvilProvider<RootProvider<Ethereum>>,
}

pub struct RollupChain {
    pub provider: RootProvider<Ethereum>,
    pub anvil: AnvilProvider<RootProvider>,
}

pub struct SimulatorTask {
    sim_env_tx: watch::Sender<Option<SimEnv>>,
    sim_env_rx: watch::Receiver<Option<SimEnv>>,
    sim_cache: SimCache,
}

pub struct TestHarness {
    pub config: BuilderConfig,
    pub constants: SignetSystemConstants,
    pub rollup: RollupChain,
    pub host: HostChain,
    pub simulator: SimulatorTask,
    submit_tx: mpsc::UnboundedSender<SimResult>,
    submit_rx: mpsc::UnboundedReceiver<SimResult>,
    /// Keeps the simulator task alive for the duration of the harness so the
    /// background task isn't aborted when `start()` returns.
    simulator_handle: Option<JoinHandle<()>>,
}

impl TestHarness {
    /// Create a new harness with a fresh Anvil rollup chain and default test config.
    pub async fn new() -> eyre::Result<Self> {
        setup_logging();

        let mut config = setup_test_config()?;

        // Ensure the slot calculator is aligned with the current time and has
        // a sufficiently large slot duration so the simulator's deadline
        // calculation yields a non-zero remaining window during tests.
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        // Give a generous slot duration for tests (60s)
        config.slot_calculator = init4_bin_base::utils::calc::SlotCalculator::new(now, 0, 10);
        let constants = SignetSystemConstants::pecorino();

        // Create host anvil
        let host_anvil = Anvil::new().chain_id(signet_constants::pecorino::HOST_CHAIN_ID).spawn();
        let host_provider = RootProvider::<Ethereum>::new_http(host_anvil.endpoint_url().clone());
        let host_anvil_provider = AnvilProvider::new(host_provider.clone(), Arc::new(host_anvil));

        // Create rollup anvil
        let rollup_anvil = Anvil::new().chain_id(signet_constants::pecorino::RU_CHAIN_ID).spawn();
        let rollup_provider =
            RootProvider::<Ethereum>::new_http(rollup_anvil.endpoint_url().clone());
        let rollup_anvil_provider =
            AnvilProvider::new(rollup_provider.clone(), Arc::new(rollup_anvil));

        let (sim_env_tx, sim_env_rx) = watch::channel::<Option<SimEnv>>(None);
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<SimResult>();

        Ok(Self {
            config,
            constants,
            rollup: RollupChain {
                provider: rollup_provider.clone(),
                anvil: rollup_anvil_provider.clone(),
            },
            host: HostChain { provider: host_provider.clone(), anvil: host_anvil_provider },
            simulator: SimulatorTask { sim_env_tx, sim_env_rx, sim_cache: SimCache::new() },
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
    pub async fn advance_blocks(&self, count: u64) -> eyre::Result<()> {
        if count == 0 {
            return Ok(());
        }

        self.host.anvil.anvil_mine(Some(count), Some(1)).await?;
        self.rollup.anvil.anvil_mine(Some(count), Some(1)).await?;

        let (ru, host) = self.get_headers().await;
        self.tick_sim_env(ru, host).await;

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

        // Wire up the simulator and submit channels
        let submit_tx = self.submit_tx.clone();
        let simulator = Simulator::new(
            &self.config,
            self.rollup.provider.clone(),
            self.simulator.sim_env_rx.clone(),
        );

        // Keep the JoinHandle on the harness so the task isn't aborted when
        // this function returns.
        let jh = simulator.spawn_simulator_task(constants, cache, submit_tx);
        self.simulator_handle = Some(jh);
        tracing::debug!("TestHarness spawned simulator task");
    }

    /// Returns the latest rollup and host headers.
    pub async fn get_headers(&self) -> (Header, Header) {
        let ru_header = self
            .rollup
            .provider
            .get_block(BlockId::latest())
            .await
            .expect("rollup latest block")
            .expect("rollup latest exists")
            .header
            .inner;

        let host_header = self
            .host
            .provider
            .get_block(BlockId::latest())
            .await
            .expect("host latest block")
            .expect("host latest exists")
            .header
            .inner;

        (ru_header, host_header)
    }   

    /// Tick a new SimEnv computed from the current host latest header.
    pub async fn update_host_environment(&self) {
        let header = self
            .host
            .provider
            .get_block(BlockId::latest())
            .await
            .expect("host latest block")
            .expect("host latest exists")
            .header
            .inner;

        let target_block_number = header.number + 1;
        let deadline = header.timestamp + DEFAULT_BLOCK_TIME;
        let block_env = test_block_env(self.config.clone(), target_block_number, 7, deadline);

        assert!(deadline > header.timestamp);
        assert!(header.number < target_block_number);

        // Re-use host header as prev_host for harness; submit path doesn't run here.
        let span =
            tracing::info_span!("TestHarness::tick_from_host", target_block_number, deadline);
        let sim_env = SimEnv { block_env, prev_header: header.clone(), prev_host: header, span };

        let _ = self.simulator.sim_env_tx.send(Some(sim_env));
    }

    /// Tick a new `SimEnv` computed from the current latest rollup and host headers.
    pub async fn tick_sim_env(&self, prev_ru_header: Header, prev_host_header: Header) {
        let target_ru_block_number = prev_ru_header.number + 1;

        // Set new simulation deadline from previous header 
        let deadline =
            prev_ru_header.timestamp + self.config.slot_calculator.slot_duration();

        // Make a new block env from the previous rollup header and our new simulation deadline.
        let block_env = test_block_env(self.config.clone(), target_ru_block_number, 7, deadline);

        let span = tracing::info_span!("TestHarness::tick", target_ru_block_number, deadline);

        // Make a new SimEnv and send it to the simulator task.
        let sim_env = SimEnv { block_env, prev_header: prev_ru_header.clone(), prev_host: prev_host_header, span };

        let _ = self.simulator.sim_env_tx.send(Some(sim_env));
    }

    /// Receive the next SimResult with a timeout.
    pub async fn recv_result(&mut self, timeout: Duration) -> Option<SimResult> {
        tracing::debug!(?timeout, "TestHarness waiting for sim result");
        let res = tokio::time::timeout(timeout, self.submit_rx.recv()).await.ok().flatten();
        tracing::debug!(received = res.is_some(), "TestHarness recv_result returning");
        res
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        if let Some(handle) = self.simulator_handle.take() {
            handle.abort();
        }
    }
}
