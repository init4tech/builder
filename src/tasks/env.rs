use crate::config::{BuilderConfig, HostProvider, RuProvider};
use alloy::{
    consensus::Header,
    eips::{BlockId, BlockNumberOrTag, eip1559::BaseFeeParams},
    primitives::{B256, U256},
    providers::Provider,
};
use init4_bin_base::deps::tracing::{self, Instrument, debug, error, info_span};
use std::time::Duration;
use tokio::{sync::watch, task::JoinHandle};
use tokio_stream::StreamExt;
use trevm::revm::{context::BlockEnv, context_interface::block::BlobExcessGasAndPrice};

/// A task that constructs a BlockEnv for the next block in the rollup chain.
#[derive(Debug, Clone)]
pub struct EnvTask {
    /// Builder configuration values.
    config: BuilderConfig,
    /// Rollup provider is used to get the latest rollup block header for simulation.
    ru_provider: RuProvider,
    /// Host provider is used to get the previous block header for gas estimation.
    host_provider: HostProvider,
}

/// Contains a signet BlockEnv and its corresponding host Header.
#[derive(Debug, Clone)]
pub struct SimEnv {
    /// The signet block environment, for rollup block simulation.
    pub signet: BlockEnv,
    /// The host environment header, for host transaction submission pricing.
    pub host: Header,
}

impl EnvTask {
    /// Create a new [`EnvTask`] with the given config and providers.
    pub const fn new(
        config: BuilderConfig,
        ru_provider: RuProvider,
        host_provider: HostProvider,
    ) -> Self {
        Self { config, ru_provider, host_provider }
    }

    /// Construct a [`BlockEnv`] by from the previous block header.
    fn construct_block_env(&self, previous: &Header) -> BlockEnv {
        BlockEnv {
            number: previous.number + 1,
            beneficiary: self.config.builder_rewards_address,
            // NB: EXACTLY the same as the previous block
            timestamp: previous.number + self.config.slot_calculator.slot_duration(),
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
        let mut poller = match self.ru_provider.watch_blocks().instrument(span.clone()).await {
            Ok(poller) => poller,
            Err(err) => {
                let _span = span.enter();
                error!(%err, "Failed to watch blocks");
                return;
            }
        };

        poller.set_poll_interval(Duration::from_millis(250));

        let mut blocks = poller.into_stream();

        while let Some(blocks) =
            blocks.next().instrument(info_span!("EnvTask::task_fut::stream")).await
        {
            let Some(block_hash) = blocks.last() else {
                // This case occurs when there are no changes to the block,
                // so we do nothing.
                continue;
            };
            let span =
                info_span!("EnvTask::task_fut::loop", %block_hash, number = tracing::field::Empty);

            // Get the rollup header for rollup block simulation environment configuration
            let rollup_header =
                match self.get_latest_rollup_header(&sender, block_hash, &span).await {
                    Some(value) => value,
                    None => continue,
                };
            debug!(?rollup_header.number, "pulled rollup block for simulation");

            // Get the host header for blob transaction submission gas pricing
            let host_header = match self.get_host_header().await {
                Ok(header) => header,
                Err(_) => {
                    error!("failed to get host header - skipping block");
                    continue;
                }
            };
            debug!(?host_header.base_fee_per_gas, "pulled previous host header for gas calculation");
            span.record("rollup_block_number", rollup_header.number);

            // Construct the block env using the previous block header
            let signet_env = self.construct_block_env(&host_header);
            debug!(
                block_number = signet_env.number,
                signet_env.basefee, "constructed signet block env"
            );

            if sender.send(Some(SimEnv { signet: signet_env, host: host_header })).is_err() {
                // The receiver has been dropped, so we can stop the task.
                debug!("receiver dropped, stopping task");
                break;
            }
        }
    }

    /// Gets the latest host [`Header`].
    /// NB: This doesn't need to correlate perfectly with the rollup blocks,
    /// since we only use the previous host block [`Header`] for gas estimation.
    async fn get_host_header(&self) -> eyre::Result<Header> {
        let previous = self
            .host_provider
            .get_block(BlockId::Number(BlockNumberOrTag::Latest))
            .into_future()
            .await?;
        debug!(?previous, "got host block for hash");

        match previous {
            Some(block) => Ok(block.header.inner),
            None => Err(eyre::eyre!("host block not found")),
        }
    }

    /// Get latest rollup [`Header`] for the given block hash.
    async fn get_latest_rollup_header(
        &self,
        sender: &watch::Sender<Option<SimEnv>>,
        block: &alloy::primitives::FixedBytes<32>,
        span: &tracing::Span,
    ) -> Option<Header> {
        let previous = match self
            .ru_provider
            .get_block((*block).into())
            .into_future()
            .instrument(span.clone())
            .await
        {
            Ok(Some(block)) => block.header.inner,
            Ok(None) => {
                let _span = span.enter();
                let _ = sender.send(None);
                debug!("rollup block not found");
                // This may mean the chain had a rollback, so the next poll
                // should find something.
                return None;
            }
            Err(err) => {
                let _span = span.enter();
                let _ = sender.send(None);
                error!(%err, "Failed to get latest block");
                // Error may be transient, so we should not break the loop.
                return None;
            }
        };
        Some(previous)
    }

    /// Spawn the task and return a watch::Receiver for the BlockEnv.
    pub fn spawn(self) -> (watch::Receiver<Option<SimEnv>>, JoinHandle<()>) {
        let (sender, receiver) = watch::channel(None);
        let fut = self.task_fut(sender);
        let jh = tokio::spawn(fut);

        (receiver, jh)
    }
}
