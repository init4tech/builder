use crate::config::{BuilderConfig, HostProvider, RuProvider};
use alloy::{
    consensus::Header,
    eips::eip1559::BaseFeeParams,
    primitives::{B256, U256},
    providers::Provider,
};
use init4_bin_base::deps::tracing::{Instrument, debug, error, info_span};
use tokio::{sync::watch, task::JoinHandle};
use tokio_stream::StreamExt;
use tracing::warn;
use trevm::revm::{context::BlockEnv, context_interface::block::BlobExcessGasAndPrice};

/// A task that constructs a BlockEnv for the next block in the rollup chain.
#[derive(Debug, Clone)]
pub struct EnvTask {
    /// Builder configuration values.
    config: BuilderConfig,

    /// Host provider is used to get the latest host block header for
    /// constructing the next block environment.
    host_provider: HostProvider,

    /// Rollup provider is used to get the latest rollup block header for
    /// simulation.
    ru_provider: RuProvider,
}

/// Contains a signet BlockEnv and its corresponding host Header.
#[derive(Debug, Clone)]
pub struct SimEnv {
    /// The signet block environment, for rollup block simulation.
    pub block_env: BlockEnv,
    /// The header of the previous rollup block.
    pub prev_header: Header,
    /// The header of the previous host block.
    pub prev_host: Header,
    /// A tracing span associated with this block
    pub span: tracing::Span,
}

impl SimEnv {
    /// Returns the block number of the signet block environment.
    pub const fn block_number(&self) -> u64 {
        self.prev_header.number.saturating_add(1)
    }

    /// Returns the host block number for the signet block environment.
    pub const fn host_block_number(&self) -> u64 {
        self.prev_host.number.saturating_add(1)
    }

    /// Returns a reference to the tracing span associated with this block env.
    pub const fn span(&self) -> &tracing::Span {
        &self.span
    }

    /// Clones the span for use in other tasks.
    pub fn clone_span(&self) -> tracing::Span {
        self.span.clone()
    }
}

impl EnvTask {
    /// Create a new [`EnvTask`] with the given config and providers.
    pub const fn new(
        config: BuilderConfig,
        host_provider: HostProvider,
        ru_provider: RuProvider,
    ) -> Self {
        Self { config, host_provider, ru_provider }
    }

    /// Construct a [`BlockEnv`] by from the previous block header.
    fn construct_block_env(&self, previous: &Header) -> BlockEnv {
        BlockEnv {
            number: U256::from(previous.number + 1),
            beneficiary: self.config.builder_rewards_address,
            // NB: EXACTLY the same as the previous block
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
        }
    }

    /// Returns a sender that sends [`SimEnv`] for communicating the next block environment.
    async fn task_fut(self, sender: watch::Sender<Option<SimEnv>>) {
        let span = info_span!("EnvTask::task_fut::init");

        let mut headers = match self.ru_provider.subscribe_blocks().await {
            Ok(poller) => poller,
            Err(err) => {
                span_scoped!(span, error!(%err, "Failed to subscribe to blocks"));
                return;
            }
        }
        .into_stream();

        drop(span);

        while let Some(rollup_header) =
            headers.next().instrument(info_span!("EnvTask::task_fut::stream")).await
        {
            let host_block_number =
                self.config.constants.rollup_block_to_host_block_num(rollup_header.number);

            let span = info_span!("SimEnv", %host_block_number, %rollup_header.hash, %rollup_header.number);

            let host_block_opt = res_unwrap_or_continue!(
                self.host_provider.get_block_by_number(host_block_number.into()).await,
                span,
                error!("error fetching previous host block - skipping block submission")
            );
            let prev_host = opt_unwrap_or_continue!(
                host_block_opt,
                span,
                warn!("previous host block not found - skipping block submission")
            )
            .header
            .inner;

            // Construct the block env using the previous block header
            let signet_env = self.construct_block_env(&rollup_header);
            debug!(
                signet_env_number = signet_env.number.to::<u64>(),
                signet_env_basefee = signet_env.basefee,
                "constructed signet block env"
            );

            if sender
                .send(Some(SimEnv {
                    span,
                    block_env: signet_env,
                    prev_header: rollup_header.inner,
                    prev_host,
                }))
                .is_err()
            {
                // The receiver has been dropped, so we can stop the task.
                debug!("receiver dropped, stopping task");
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
