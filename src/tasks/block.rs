use super::bundler::{Bundle, BundlePoller};
use super::oauth::Authenticator;
use super::tx_poller::TxPoller;
use crate::config::BuilderConfig;
use alloy::providers::Provider;
use alloy::{
    consensus::{SidecarBuilder, SidecarCoder, TxEnvelope},
    eips::eip2718::Decodable2718,
};
use alloy_primitives::{keccak256, Bytes, B256};
use alloy_rlp::Buf;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{sync::OnceLock, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::Instrument;
use zenith_types::{encode_txns, Alloy2718Coder};

/// Ethereum's slot time in seconds.
pub const ETHEREUM_SLOT_TIME: u64 = 12;

#[derive(Debug, Default, Clone)]
/// A block in progress.
pub struct InProgressBlock {
    transactions: Vec<TxEnvelope>,
    raw_encoding: OnceLock<Bytes>,
    hash: OnceLock<B256>,
}

impl InProgressBlock {
    /// Create a new `InProgressBlock`
    pub fn new() -> Self {
        Self { transactions: Vec::new(), raw_encoding: OnceLock::new(), hash: OnceLock::new() }
    }

    /// Get the number of transactions in the block.
    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    /// Check if the block is empty.
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }

    /// Unseal the block
    fn unseal(&mut self) {
        self.raw_encoding.take();
        self.hash.take();
    }

    /// Seal the block by encoding the transactions and calculating the contentshash.
    fn seal(&self) {
        self.raw_encoding.get_or_init(|| encode_txns::<Alloy2718Coder>(&self.transactions).into());
        self.hash.get_or_init(|| keccak256(self.raw_encoding.get().unwrap().as_ref()));
    }

    /// Ingest a transaction into the in-progress block. Fails
    pub fn ingest_tx(&mut self, tx: &TxEnvelope) {
        tracing::info!(hash = %tx.tx_hash(), "ingesting tx");
        self.unseal();
        self.transactions.push(tx.clone());
    }

    /// Remove a transaction from the in-progress block.
    pub fn remove_tx(&mut self, tx: &TxEnvelope) {
        tracing::info!(hash = %tx.tx_hash(), "removing tx");
        self.unseal();
        self.transactions.retain(|t| t.tx_hash() != tx.tx_hash());
    }

    /// Ingest a bundle into the in-progress block.
    /// Ignores Signed Orders for now.
    pub fn ingest_bundle(&mut self, bundle: Bundle) {
        tracing::info!(bundle = %bundle.id, "ingesting bundle");

        let txs = bundle
            .bundle
            .bundle
            .txs
            .into_iter()
            .map(|tx| TxEnvelope::decode_2718(&mut tx.chunk()))
            .collect::<Result<Vec<_>, _>>();

        if let Ok(txs) = txs {
            self.unseal();
            // extend the transactions with the decoded transactions.
            // As this builder does not provide bundles landing "top of block", its fine to just extend.
            self.transactions.extend(txs);
        } else {
            tracing::error!("failed to decode bundle. dropping");
        }
    }

    /// Encode the in-progress block
    fn encode_raw(&self) -> &Bytes {
        self.seal();
        self.raw_encoding.get().unwrap()
    }

    /// Calculate the hash of the in-progress block, finishing the block.
    pub fn contents_hash(&self) -> alloy_primitives::B256 {
        self.seal();
        *self.hash.get().unwrap()
    }

    /// Convert the in-progress block to sign request contents.
    pub fn encode_calldata(&self) -> &Bytes {
        self.encode_raw()
    }

    /// Convert the in-progress block to a blob transaction sidecar.
    pub fn encode_blob<T: SidecarCoder + Default>(&self) -> SidecarBuilder<T> {
        let mut coder = SidecarBuilder::<T>::default();
        coder.ingest(self.encode_raw());
        coder
    }
}

/// BlockBuilder is a task that periodically builds a block then sends it for signing and submission.
pub struct BlockBuilder {
    pub config: BuilderConfig,
    pub ru_provider: crate::config::Provider,
    pub tx_poller: TxPoller,
    pub bundle_poller: BundlePoller,
}

impl BlockBuilder {
    // create a new block builder with the given config.
    pub fn new(
        config: &BuilderConfig,
        authenticator: Authenticator,
        ru_provider: crate::config::Provider,
    ) -> Self {
        Self {
            config: config.clone(),
            ru_provider,
            tx_poller: TxPoller::new(config),
            bundle_poller: BundlePoller::new(config, authenticator),
        }
    }

    async fn get_transactions(&mut self, in_progress: &mut InProgressBlock) {
        tracing::info!("query transactions from cache");
        let txns = self.tx_poller.check_tx_cache().await;
        match txns {
            Ok(txns) => {
                tracing::info!("got transactions response");
                for txn in txns.into_iter() {
                    in_progress.ingest_tx(&txn);
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "error polling transactions");
            }
        }
        self.tx_poller.evict();
    }

    async fn _get_bundles(&mut self, in_progress: &mut InProgressBlock) {
        tracing::info!("query bundles from cache");
        let bundles = self.bundle_poller.check_bundle_cache().await;
        match bundles {
            Ok(bundles) => {
                tracing::info!("got bundles response");
                for bundle in bundles {
                    in_progress.ingest_bundle(bundle);
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "error polling bundles");
            }
        }
        self.bundle_poller.evict();
    }

    async fn filter_transactions(&mut self, in_progress: &mut InProgressBlock) {
        // query the rollup node to see which transaction(s) have been included
        let mut confirmed_transactions = Vec::new();
        for transaction in in_progress.transactions.iter() {
            let tx = self
                .ru_provider
                .get_transaction_by_hash(*transaction.tx_hash())
                .await
                .expect("failed to get receipt");
            if tx.is_some() {
                confirmed_transactions.push(transaction.clone());
            }
        }
        tracing::info!(confirmed = confirmed_transactions.len(), "found confirmed transactions");

        // remove already-confirmed transactions
        for transaction in confirmed_transactions {
            in_progress.remove_tx(&transaction);
        }
    }

    // calculate the duration in seconds until the beginning of the next block slot.
    fn secs_to_next_slot(&mut self) -> u64 {
        let curr_timestamp: u64 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let current_slot_time = (curr_timestamp - self.config.chain_offset) % ETHEREUM_SLOT_TIME;
        (ETHEREUM_SLOT_TIME - current_slot_time) % ETHEREUM_SLOT_TIME
    }

    // add a buffer to the beginning of the block slot.
    fn secs_to_next_target(&mut self) -> u64 {
        self.secs_to_next_slot() + self.config.target_slot_time
    }

    /// Spawn the block builder task, returning the inbound channel to it, and
    /// a handle to the running task.
    pub fn spawn(mut self, outbound: mpsc::UnboundedSender<InProgressBlock>) -> JoinHandle<()> {
        tokio::spawn(
            async move {
                loop {
                    // sleep the buffer time
                    tokio::time::sleep(Duration::from_secs(self.secs_to_next_target())).await;
                    tracing::info!("beginning block build cycle");

                    // Build a block
                    let mut in_progress = InProgressBlock::default();
                    self.get_transactions(&mut in_progress).await;

                    // TODO: Implement bundle ingestion #later
                    // self.get_bundles(&mut in_progress).await;

                    // Filter confirmed transactions from the block
                    self.filter_transactions(&mut in_progress).await;

                    // submit the block if it has transactions
                    if !in_progress.is_empty() {
                        tracing::info!(txns = in_progress.len(), "sending block to submit task");
                        let in_progress_block = std::mem::take(&mut in_progress);
                        if outbound.send(in_progress_block).is_err() {
                            tracing::debug!("downstream task gone");
                            break;
                        }
                    } else {
                        tracing::info!("no transactions, skipping block submission");
                    }
                }
            }
            .in_current_span(),
        )
    }
}
