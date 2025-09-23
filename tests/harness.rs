//! Test harness for end-to-end simulation flow.
//!
//! This harness wires a Simulator to a manual SimEnv watch channel and an
//! mpsc submit channel so tests can tick BlockEnv values and assert on
//! emitted SimResult blocks without involving submission logic.

use std::time::Duration;

use alloy::{
    eips::BlockId,
    network::Ethereum,
    node_bindings::{Anvil, AnvilInstance},
    primitives::U256,
    providers::{Provider, RootProvider},
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
use tokio::sync::{mpsc, watch};

pub struct TestHarness {
    pub config: BuilderConfig,
    pub constants: SignetSystemConstants,
    pub anvil: AnvilInstance,
    pub ru_provider: RootProvider<Ethereum>,
    sim_env_tx: watch::Sender<Option<SimEnv>>,
    sim_env_rx: watch::Receiver<Option<SimEnv>>,
    submit_tx: mpsc::UnboundedSender<SimResult>,
    submit_rx: mpsc::UnboundedReceiver<SimResult>,
    sim_cache: SimCache,
}

impl TestHarness {
    /// Create a new harness with a fresh Anvil rollup chain and default test config.
    pub async fn new() -> eyre::Result<Self> {
        setup_logging();

        let config = setup_test_config()?;
        let constants = SignetSystemConstants::pecorino();

        let anvil = Anvil::new().chain_id(signet_constants::pecorino::RU_CHAIN_ID).spawn();
        let ru_provider = RootProvider::<Ethereum>::new_http(anvil.endpoint_url());

        let (sim_env_tx, sim_env_rx) = watch::channel::<Option<SimEnv>>(None);
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<SimResult>();

        Ok(Self {
            config,
            constants,
            anvil,
            ru_provider,
            sim_env_tx,
            sim_env_rx,
            submit_tx,
            submit_rx,
            sim_cache: SimCache::new(),
        })
    }

    /// Add a signed transaction from a provided signer to the sim cache.
    pub fn add_tx(&self, signer: &PrivateKeySigner, nonce: u64, value: U256, mpfpg: u128) {
        let tx = new_signed_tx(signer, nonce, value, mpfpg).expect("tx signing");
        // group index 0 for simplicity in tests
        self.sim_cache.add_tx(tx, 0);
    }

    /// Start the simulator task.
    pub fn start(&self) {
        // Spawn the simulator background task
        let cache = self.sim_cache.clone();
        let constants = self.constants.clone();
        let submit_tx = self.submit_tx.clone();
        let simulator =
            Simulator::new(&self.config, self.ru_provider.clone(), self.sim_env_rx.clone());
        let _jh = simulator.spawn_simulator_task(constants, cache, submit_tx);
        // NB: We intentionally leak the JoinHandle in tests; tokio will cancel on drop.
    }

    /// Tick a new SimEnv computed from the current RU latest header.
    pub async fn tick_from_ru_latest(&self) {
        let header = self
            .ru_provider
            .get_block(BlockId::latest())
            .await
            .expect("ru latest block")
            .expect("ru latest exists")
            .header
            .inner;

        let number = header.number + 1;
        let timestamp = header.timestamp + self.config.slot_calculator.slot_duration();
        // A small basefee is fine for local testing
        let block_env = test_block_env(self.config.clone(), number, 7, timestamp);

        // Re-use RU header as prev_host for harness; submit path doesn't run here.
        let span = tracing::info_span!("TestHarness::tick", number, timestamp);
        let sim_env = SimEnv { block_env, prev_header: header.clone(), prev_host: header, span };

        let _ = self.sim_env_tx.send(Some(sim_env));
    }

    /// Receive the next SimResult with a timeout.
    pub async fn recv_result(&mut self, timeout: Duration) -> Option<SimResult> {
        tokio::time::timeout(timeout, self.submit_rx.recv()).await.ok().flatten()
    }
}
