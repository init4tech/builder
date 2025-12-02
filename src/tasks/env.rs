use crate::{
    config::{BuilderConfig, HostProvider, RuProvider},
    quincey::Quincey,
    tasks::block::cfg::SignetCfgEnv,
};
use alloy::{
    consensus::Header,
    eips::eip1559::BaseFeeParams,
    network::Ethereum,
    primitives::{B256, U256},
    providers::{Provider, network::Network},
};
use signet_constants::SignetSystemConstants;
use signet_sim::{HostEnv, RollupEnv};
use tokio::{sync::watch, task::JoinHandle};
use tokio_stream::StreamExt;
use tracing::{Instrument, Span, info_span, instrument};
use trevm::revm::{
    context::BlockEnv,
    context_interface::block::BlobExcessGasAndPrice,
    database::{AlloyDB, WrapDatabaseAsync},
    inspector::NoOpInspector,
};

/// Type aliases for database providers.
pub type HostAlloyDatabaseProvider = WrapDatabaseAsync<AlloyDB<Ethereum, HostProvider>>;
/// Type aliases for database providers.
pub type RollupAlloyDatabaseProvider = WrapDatabaseAsync<AlloyDB<Ethereum, RuProvider>>;

/// Type aliases for simulation environments.
pub type SimRollupEnv = RollupEnv<RollupAlloyDatabaseProvider, NoOpInspector>;
/// Type aliases for simulation environments.
pub type SimHostEnv = HostEnv<HostAlloyDatabaseProvider, NoOpInspector>;

/// An environment for simulating a block.
#[derive(Debug, Clone)]
pub struct Environment {
    block_env: BlockEnv,
    prev_header: Header,
}

impl Environment {
    /// Create a new `Environment` with the given block environment and
    /// previous header.
    pub const fn new(block_env: BlockEnv, prev_header: Header) -> Self {
        Self { block_env, prev_header }
    }

    /// Get a reference to the block environment.
    pub const fn block_env(&self) -> &BlockEnv {
        &self.block_env
    }

    /// Get a reference to the previous block header.
    pub const fn prev_header(&self) -> &Header {
        &self.prev_header
    }

    /// Create a new empty `Environment` for testing purposes.
    #[doc(hidden)]
    pub fn for_testing() -> Self {
        Self { block_env: Default::default(), prev_header: Header::default() }
    }

    /// Create a new [`AlloyDB`] for this environment using the given provider.
    pub fn alloy_db<N: Network, P: Provider<N>>(&self, provider: P) -> AlloyDB<N, P> {
        AlloyDB::new(provider, self.prev_header.number.into())
    }
}

/// Contains environments to simulate both host and rollup blocks.
#[derive(Debug, Clone)]
pub struct SimEnv {
    /// The host environment, for host block simulation.
    pub host: Environment,

    /// The rollup environment, for rollup block simulation.
    pub rollup: Environment,

    /// A tracing span associated with this block simulation.
    pub span: Span,
}

impl SimEnv {
    /// Get a reference to previous rollup header.
    pub const fn prev_rollup(&self) -> &Header {
        &self.rollup.prev_header
    }

    /// Get a reference to the previous host header.
    pub const fn prev_host(&self) -> &Header {
        &self.host.prev_header
    }

    /// Get the block number of the rollup block environment.
    pub const fn prev_rollup_block_number(&self) -> u64 {
        self.prev_rollup().number
    }

    /// Get the block number for the host block environment.
    pub const fn prev_host_block_number(&self) -> u64 {
        self.prev_host().number
    }

    /// Get the block number of the rollup block environment.
    pub const fn rollup_block_number(&self) -> u64 {
        self.prev_rollup().number.saturating_add(1)
    }

    /// Get the block number for the host block environment.
    pub const fn host_block_number(&self) -> u64 {
        self.prev_host().number.saturating_add(1)
    }

    /// Get a reference to the rollup block environment.
    pub const fn rollup_env(&self) -> &BlockEnv {
        &self.rollup.block_env
    }

    /// Get a reference to the host block environment.
    pub const fn host_env(&self) -> &BlockEnv {
        &self.host.block_env
    }

    /// Get a reference to the tracing span associated with this block env.
    pub const fn span(&self) -> &Span {
        &self.span
    }

    /// Clones the span for use in other tasks.
    pub fn clone_span(&self) -> Span {
        self.span.clone()
    }

    /// Create an [`AlloyDB`] for the rollup environment using the given
    /// provider.
    ///
    /// # Panics
    ///
    /// This function will panic if not called within a Tokio runtime.
    pub fn rollup_db(&self, provider: RuProvider) -> RollupAlloyDatabaseProvider {
        WrapDatabaseAsync::new(self.rollup.alloy_db(provider)).expect("in tokio runtime")
    }

    /// Create an [`AlloyDB`] for the host environment using the given provider.
    ///
    /// # Panics
    ///
    /// This function will panic if not called within a Tokio runtime.
    pub fn host_db(&self, provider: HostProvider) -> HostAlloyDatabaseProvider {
        WrapDatabaseAsync::new(self.host.alloy_db(provider)).expect("in tokio runtime")
    }

    /// Create a simulated rollup environment using the given provider,
    /// constants, and configuration.
    ///
    /// # Panics
    ///
    /// This function will panic if not called within a Tokio runtime.
    pub fn sim_rollup_env(
        &self,
        constants: &SignetSystemConstants,
        provider: RuProvider,
    ) -> SimRollupEnv {
        let rollup_cfg = SignetCfgEnv { chain_id: constants.ru_chain_id() };
        RollupEnv::new(self.rollup_db(provider), constants.clone(), &rollup_cfg, self.rollup_env())
    }

    /// Create a simulated host environment using the given provider,
    /// constants, and configuration.
    ///
    /// # Panics
    ///
    /// This function will panic if not called within a Tokio runtime.
    pub fn sim_host_env(
        &self,
        constants: &SignetSystemConstants,
        provider: HostProvider,
    ) -> SimHostEnv {
        let host_cfg = SignetCfgEnv { chain_id: constants.host_chain_id() };
        HostEnv::new(self.host_db(provider), constants.clone(), &host_cfg, self.host_env())
    }
}

/// A task that constructs a BlockEnv for the next block in the rollup chain.
#[derive(Debug, Clone)]
pub struct EnvTask {
    /// Builder configuration values.
    config: &'static BuilderConfig,

    /// Host provider is used to get the latest host block header for
    /// constructing the next block environment.
    host_provider: HostProvider,

    /// Quincey instance for slot checking.
    quincey: Quincey,

    /// Rollup provider is used to get the latest rollup block header for
    /// simulation.
    ru_provider: RuProvider,
}

impl EnvTask {
    /// Create a new [`EnvTask`] with the given config and providers.
    pub async fn new() -> eyre::Result<Self> {
        let config = crate::config();

        let (host_provider, quincey, ru_provider) = tokio::try_join!(
            config.connect_host_provider(),
            config.connect_quincey(),
            config.connect_ru_provider(),
        )?;

        Ok(Self { config, host_provider, quincey, ru_provider })
    }

    /// Construct a [`BlockEnv`] for the next host block from the previous host header.
    #[instrument(skip(self, previous), fields(previous_number = %previous.number))]
    pub fn construct_host_env(&self, previous: Header) -> Environment {
        let env = BlockEnv {
            number: U256::from(previous.number + 1),
            beneficiary: self.config.builder_rewards_address,
            // NB: EXACTLY the same as the previous block timestamp + slot duration
            timestamp: U256::from(previous.timestamp + self.config.slot_calculator.slot_duration()),
            gas_limit: self.config.max_host_gas(previous.gas_limit),
            basefee: previous
                .next_block_base_fee(BaseFeeParams::ethereum())
                .expect("signet has no non-1559 headers"),
            difficulty: U256::ZERO,
            prevrandao: Some(B256::random()),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
                excess_blob_gas: 0,
                blob_gasprice: 0,
            }),
        };
        Environment::new(env, previous)
    }

    /// Construct a [`BlockEnv`] for the next rollup block from the previous block header.
    #[instrument(skip(self, previous), fields(previous_number = %previous.number))]
    pub fn construct_rollup_env(&self, previous: Header) -> Environment {
        let env = BlockEnv {
            number: U256::from(previous.number + 1),
            beneficiary: self.config.builder_rewards_address,
            // NB: EXACTLY the same as the previous block timestamp + slot duration
            timestamp: U256::from(previous.timestamp + self.config.slot_calculator.slot_duration()),
            gas_limit: self.config.rollup_block_gas_limit,
            basefee: previous
                .next_block_base_fee(BaseFeeParams::ethereum())
                .expect("signet has no non-1559 headers"),
            difficulty: U256::ZERO,
            prevrandao: Some(B256::random()),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
                excess_blob_gas: 0,
                blob_gasprice: 0,
            }),
        };
        Environment::new(env, previous)
    }

    /// Returns a sender that sends [`SimEnv`] for communicating the next block environment.
    async fn task_fut(self, sender: watch::Sender<Option<SimEnv>>) {
        let span = info_span!("EnvTask::task_fut::init");

        let mut rollup_headers = match self.ru_provider.subscribe_blocks().await {
            Ok(poller) => poller,
            Err(err) => {
                span_error!(span, %err, "Failed to subscribe to blocks");
                return;
            }
        }
        .into_stream();

        drop(span);

        while let Some(rollup_header) =
            rollup_headers.next().instrument(info_span!("EnvTask::task_fut::stream")).await
        {
            let host_block_number =
                self.config.constants.rollup_block_to_host_block_num(rollup_header.number);

            let span = info_span!("SimEnv", %host_block_number, %rollup_header.hash, %rollup_header.number);

            let (host_block_res, quincey_res) = tokio::join!(
                self.host_provider.get_block_by_number(host_block_number.into()),
                // We want to check that we're able to sign for the block we're gonna start building.
                // If not, we just want to skip all the work.
                self.quincey.preflight_check(host_block_number + 1)
            );

            res_unwrap_or_continue!(
                quincey_res,
                span,
                error!("error checking quincey slot - skipping block submission"),
            );

            let host_block_opt = res_unwrap_or_continue!(
                host_block_res,
                span,
                error!("error fetching previous host block - skipping block submission")
            );

            let host_header = opt_unwrap_or_continue!(
                host_block_opt,
                span,
                warn!("previous host block not found - skipping block submission")
            )
            .header
            .inner;

            if rollup_header.timestamp != host_header.timestamp {
                span_warn!(
                    span,
                    rollup_timestamp = rollup_header.timestamp,
                    host_timestamp = host_header.timestamp,
                    "rollup block timestamp differs from host block timestamp. - skipping block submission"
                );
                continue;
            }

            // Construct the block env using the previous block header
            let rollup_env = self.construct_rollup_env(rollup_header.into());
            let host_env = self.construct_host_env(host_header);

            span_debug!(
                span,
                rollup_env_number = rollup_env.block_env.number.to::<u64>(),
                rollup_env_basefee = rollup_env.block_env.basefee,
                "constructed block env"
            );

            if sender.send(Some(SimEnv { span, rollup: rollup_env, host: host_env })).is_err() {
                // The receiver has been dropped, so we can stop the task.
                tracing::debug!("receiver dropped, stopping task");
                break;
            }
        }
    }

    /// Spawn the task and return a watch::Receiver for the BlockEnv.
    pub fn spawn(self) -> (watch::Receiver<Option<SimEnv>>, JoinHandle<()>) {
        let (sender, receiver) = watch::channel(None);
        let fut = self.task_fut(sender);
        let jh = tokio::spawn(fut);

        (receiver, jh)
    }
}
