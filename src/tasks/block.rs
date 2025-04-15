use crate::{
    config::{BuilderConfig, WalletlessProvider},
    tasks::{
        bundler::{Bundle, BundlePoller},
        oauth::Authenticator,
        tx_poller::TxPoller,
    },
};
use alloy::{consensus::TxEnvelope, eips::BlockId, genesis::Genesis};
use signet_sim::{BlockBuild, BuiltBlock, SimCache};
use signet_types::config::SignetSystemConstants;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::{select, sync::mpsc, task::JoinHandle};
use tracing::{Instrument, info};
use trevm::{
    NoopBlock, NoopCfg,
    revm::{
        database::{AlloyDB, CacheDB, WrapDatabaseAsync},
        inspector::NoOpInspector,
    },
};

/// Ethereum's slot time in seconds.
pub const ETHEREUM_SLOT_TIME: u64 = 12;

/// BlockBuilder is a task that periodically builds a block then sends it for
/// signing and submission.
#[derive(Debug)]
pub struct BlockBuilder {
    /// Configuration.
    pub config: BuilderConfig,
    /// A provider that cannot sign transactions.
    pub ru_provider: WalletlessProvider,
    /// A poller for fetching transactions.
    pub tx_poller: TxPoller,
    /// A poller for fetching bundles.
    pub bundle_poller: BundlePoller,
}

impl BlockBuilder {
    /// Create a new block builder with the given config.
    pub fn new(
        config: &BuilderConfig,
        authenticator: Authenticator,
        ru_provider: WalletlessProvider,
    ) -> Self {
        Self {
            config: config.clone(),
            ru_provider,
            tx_poller: TxPoller::new(config),
            bundle_poller: BundlePoller::new(config, authenticator),
        }
    }

    // calculate the duration in seconds until the beginning of the next block slot.
    fn secs_to_next_slot(&self) -> u64 {
        // TODO: Account for multiple builders and return the time to our next _assigned_ slot here.
        let curr_timestamp: u64 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let current_slot_time = (curr_timestamp - self.config.chain_offset) % ETHEREUM_SLOT_TIME;
        (ETHEREUM_SLOT_TIME - current_slot_time) % ETHEREUM_SLOT_TIME
    }

    // add a buffer to the beginning of the block slot.
    fn secs_to_next_target(&self) -> u64 {
        self.secs_to_next_slot() + self.config.target_slot_time
    }

    /// Spawn the block builder task, returning the inbound channel to it, and
    /// a handle to the running task.
    pub fn spawn(
        self,
        ru_provider: WalletlessProvider,
        mut tx_receiver: mpsc::UnboundedReceiver<TxEnvelope>,
        mut bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
        outbound: mpsc::UnboundedSender<BuiltBlock>,
    ) -> JoinHandle<()> {
        let genesis = Genesis::default();
        let constants = SignetSystemConstants::try_from_genesis(&genesis).unwrap(); // TODO: get from actual genesis file.
        let concurrency_limit = 1000;
        let max_gas = 30_000_000;

        tokio::spawn(
            async move {
                loop {
                    // setup during slot time dead zone
                    let alloy_db = AlloyDB::new(ru_provider.clone(), BlockId::from(1));
                    let wrapped_db = WrapDatabaseAsync::new(alloy_db).unwrap();
                    let cached_db = CacheDB::new(wrapped_db);

                    let sim_items = SimCache::new();

                    let deadline = Instant::now()
                        .checked_add(Duration::from_secs(self.secs_to_next_target()))
                        .unwrap();

                    let block_builder: BlockBuild<_, NoOpInspector> = BlockBuild::new(
                        cached_db,
                        constants,
                        NoopCfg,
                        NoopBlock,
                        deadline,
                        concurrency_limit,
                        sim_items.clone(),
                        max_gas,
                    );

                    // sleep until next buffer time
                    tokio::time::sleep(Duration::from_secs(self.secs_to_next_target())).await;
                    info!("beginning block build cycle");

                    tokio::spawn({
                        let outbound = outbound.clone();
                        async move {
                            let block = block_builder.build().await;
                            if let Err(e) = outbound.send(block) {
                                println!("failed to send built block: {}", e);
                                tracing::error!(error = %e, "failed to send built block");
                            } else {
                                info!("block build cycle complete");
                            }
                        }
                    });

                    // Feed it transactions and bundles until deadline
                    loop {
                        select! {
                            tx = tx_receiver.recv() => {
                                if let Some(tx) = tx {
                                    sim_items.add_item(tx);
                                }
                            }
                            bundle = bundle_receiver.recv() => {
                                if let Some(bundle) = bundle {
                                    sim_items.add_item(bundle.bundle);
                                }
                            }
                            _ = tokio::time::sleep_until(deadline.into()) => {
                                break;
                            }
                        }
                    }
                }
            }
            .in_current_span(),
        )
    }
}
