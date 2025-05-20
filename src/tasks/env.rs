use crate::config::{BuilderConfig, RuProvider};
use alloy::{
    consensus::Header,
    eips::eip1559::BaseFeeParams,
    primitives::{B256, U256},
    providers::Provider,
};
use init4_bin_base::deps::tracing::{self, Instrument, debug, error, info_span};
use std::time::Duration;
use tokio::sync::watch;
use tokio_stream::StreamExt;
use trevm::revm::{context::BlockEnv, context_interface::block::BlobExcessGasAndPrice};

/// A task that constructs a BlockEnv for the next block in the rollup chain.
#[derive(Debug, Clone)]
pub struct EnvTask {
    config: BuilderConfig,
    provider: RuProvider,
}

impl EnvTask {
    /// Create a new EnvTask with the given config and provider.
    pub const fn new(config: BuilderConfig, provider: RuProvider) -> Self {
        Self { config, provider }
    }

    /// Construct a BlockEnv by making calls to the provider.
    pub fn construct_block_env(&self, previous: &Header) -> BlockEnv {
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

    /// Construct the BlockEnv and send it to the sender.
    pub async fn task_fut(self, sender: watch::Sender<Option<BlockEnv>>) {
        let span = info_span!("EnvTask::task_fut::init");
        let mut poller = match self.provider.watch_blocks().instrument(span.clone()).await {
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
            let Some(block) = blocks.last() else {
                // This case occurs when there are no changes to the block,
                // so we do nothing.
                debug!("empty filter changes");
                continue;
            };
            let span = info_span!("EnvTask::task_fut::loop", hash = %block, number = tracing::field::Empty);

            let previous = match self
                .provider
                .get_block((*block).into())
                .into_future()
                .instrument(span.clone())
                .await
            {
                Ok(Some(block)) => block.header.inner,
                Ok(None) => {
                    let _span = span.enter();
                    let _ = sender.send(None);
                    debug!("block not found");
                    // This may mean the chain had a rollback, so the next poll
                    // should find something.
                    continue;
                }
                Err(err) => {
                    let _span = span.enter();
                    let _ = sender.send(None);
                    error!(%err, "Failed to get latest block");
                    // Error may be transient, so we should not break the loop.
                    continue;
                }
            };
            span.record("number", previous.number);

            let env = self.construct_block_env(&previous);
            debug!(?env, "constructed block env");
            if sender.send(Some(env)).is_err() {
                // The receiver has been dropped, so we can stop the task.
                break;
            }
        }
    }

    /// Spawn the task and return a watch::Receiver for the BlockEnv.
    pub fn spawn(self) -> watch::Receiver<Option<BlockEnv>> {
        let (sender, receiver) = watch::channel(None);
        let fut = self.task_fut(sender);
        tokio::spawn(fut);

        receiver
    }
}
