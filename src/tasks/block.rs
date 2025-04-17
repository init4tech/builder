use crate::{
    config::{BuilderConfig, WalletlessProvider},
    tasks::{
        bundler::{Bundle, BundlePoller},
        oauth::Authenticator,
        tx_poller::TxPoller,
    },
};
use alloy::{consensus::TxEnvelope, eips::BlockId, providers::Provider};
use signet_sim::{BlockBuild, BuiltBlock, SimCache, SimItem};
use signet_types::config::SignetSystemConstants;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::{
    select,
    sync::mpsc::{self, UnboundedReceiver},
    task::JoinHandle,
};
use tracing::{Instrument, info};
use trevm::{
    NoopBlock, NoopCfg,
    revm::{
        database::{AlloyDB, WrapDatabaseAsync},
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
        constants: SignetSystemConstants,
        ru_provider: WalletlessProvider,
        mut tx_receiver: UnboundedReceiver<TxEnvelope>,
        mut bundle_receiver: mpsc::UnboundedReceiver<Bundle>,
        block_sender: mpsc::UnboundedSender<BuiltBlock>,
    ) -> JoinHandle<()> {
        println!("GOT HERE 0");
        tokio::spawn(
            async move {
                // Create a sim item handler
                let sim_items = SimCache::new();
                println!("got here 1");

                tokio::spawn({
                    let sim_items = sim_items.clone();
                    async move {
                        println!("starting up the receiver");
                        loop {
                            select! {
                                tx = tx_receiver.recv() => {
                                    if let Some(tx) = tx {
                                        println!("received transaction {}", tx.hash());
                                        sim_items.add_item(signet_sim::SimItem::Tx(tx.into()));
                                    }
                                }
                                bundle = bundle_receiver.recv() => {
                                    if let Some(bundle) = bundle {
                                        println!("received bundle {}", bundle.id);
                                        sim_items.add_item(SimItem::Bundle(bundle.bundle));
                                    }
                                }
                            }
                        }
                    }
                });

                println!("starting the block builder loop");

                loop {
                    println!("STARTING 1");
                    // Calculate the next wake up
                    let buffer = self.secs_to_next_target();
                    let deadline = Instant::now().checked_add(Duration::from_secs(buffer)).unwrap();
                    println!("DEADLINE {:?}", deadline.clone());

                    tokio::time::sleep(Duration::from_secs(buffer)).await;

                    // Fetch latest block number from the rollup
                    let db = match create_db(&ru_provider).await {
                        Some(value) => value,
                        None => {
                            println!("failed to get a database - check runtime type");
                            continue;
                        }
                    };

                    println!("SIM ITEMS LEN {}", sim_items.len());

                    tokio::spawn({
                        let outbound = block_sender.clone();
                        let sim_items = sim_items.clone();

                        async move {
                            let block_builder: BlockBuild<_, NoOpInspector> = BlockBuild::new(
                                db,
                                constants,
                                NoopCfg,
                                NoopBlock,
                                deadline,
                                self.config.concurrency_limit.clone(),
                                sim_items.clone(),
                                self.config.rollup_block_gas_limit.clone(),
                            );

                            let block = block_builder.build().await;
                            println!("GOT BLOCK {}", block.contents_hash());

                            if let Err(e) = outbound.send(block) {
                                println!("failed to send built block: {}", e);
                                tracing::error!(error = %e, "failed to send built block");
                            } else {
                                info!("block build cycle complete");
                            }
                        }
                    });
                }
            }
            .in_current_span(),
        )
    }
}

/// Creates an AlloyDB from a rollup provider
async fn create_db(
    ru_provider: &alloy::providers::fillers::FillProvider<
        alloy::providers::fillers::JoinFill<
            alloy::providers::Identity,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::GasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::BlobGasFiller,
                    alloy::providers::fillers::JoinFill<
                        alloy::providers::fillers::NonceFiller,
                        alloy::providers::fillers::ChainIdFiller,
                    >,
                >,
            >,
        >,
        alloy::providers::RootProvider,
    >,
) -> Option<
    WrapDatabaseAsync<
        AlloyDB<
            alloy::network::Ethereum,
            alloy::providers::fillers::FillProvider<
                alloy::providers::fillers::JoinFill<
                    alloy::providers::Identity,
                    alloy::providers::fillers::JoinFill<
                        alloy::providers::fillers::GasFiller,
                        alloy::providers::fillers::JoinFill<
                            alloy::providers::fillers::BlobGasFiller,
                            alloy::providers::fillers::JoinFill<
                                alloy::providers::fillers::NonceFiller,
                                alloy::providers::fillers::ChainIdFiller,
                            >,
                        >,
                    >,
                >,
                alloy::providers::RootProvider,
            >,
        >,
    >,
> {
    let latest = match ru_provider.get_block_number().await {
        Ok(block_number) => block_number,
        Err(e) => {
            tracing::error!(error = %e, "failed to get latest block number");
            println!("failed to get latest block number");
            // Should this do anything else?
            return None;
        }
    };
    let alloy_db = AlloyDB::new(ru_provider.clone(), BlockId::from(latest));
    let wrapped_db = WrapDatabaseAsync::new(alloy_db).unwrap_or_else(|| {
        panic!("failed to acquire async alloy_db; check which runtime you're using")
    });
    Some(wrapped_db)
}
