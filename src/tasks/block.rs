use super::bundler::{Bundle, BundlePoller};
use super::oauth::Authenticator;
use super::tx_poller::TxPoller;
use crate::config::{BuilderConfig, WalletlessProvider};
use alloy::{
    consensus::{SidecarBuilder, SidecarCoder, transaction::TxEnvelope},
    eips::eip2718::Decodable2718,
    primitives::{B256, Bytes, keccak256},
    providers::Provider as _,
    rlp::Buf,
};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{sync::OnceLock, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{Instrument, debug, error, info, trace};
use signet_zenith::{Alloy2718Coder, encode_txns};
use signet_bundle::SignetEthBundle;

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
    pub const fn new() -> Self {
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
        trace!(hash = %tx.tx_hash(), "ingesting tx");
        self.unseal();
        self.transactions.push(tx.clone());
    }

    /// Remove a transaction from the in-progress block.
    pub fn remove_tx(&mut self, tx: &TxEnvelope) {
        trace!(hash = %tx.tx_hash(), "removing tx");
        self.unseal();
        self.transactions.retain(|t| t.tx_hash() != tx.tx_hash());
    }

    /// Ingest a bundle into the in-progress block.
    /// Ignores Signed Orders for now.
    pub fn ingest_bundle(&mut self, bundle: Bundle) {
        trace!(bundle = %bundle.id, "ingesting bundle");

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
            error!("failed to decode bundle. dropping");
        }
    }

    /// Encode the in-progress block
    fn encode_raw(&self) -> &Bytes {
        self.seal();
        self.raw_encoding.get().unwrap()
    }

    /// Calculate the hash of the in-progress block, finishing the block.
    pub fn contents_hash(&self) -> B256 {
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

    /// Fetches transactions from the cache and ingests them into the in
    /// progress block
    async fn get_transactions(&mut self, in_progress: &mut InProgressBlock) {
        trace!("query transactions from cache");
        let txns = self.tx_poller.check_tx_cache().await;
        match txns {
            Ok(txns) => {
                trace!("got transactions response");
                for txn in txns.into_iter() {
                    in_progress.ingest_tx(&txn);
                }
            }
            Err(e) => {
                error!(error = %e, "error polling transactions");
            }
        }
    }

    /// Fetches bundles from the cache and ingests them into the in progress block
    async fn get_bundles(&mut self, in_progress: &mut InProgressBlock) {
        trace!("query bundles from cache");
        let bundles = self.bundle_poller.check_bundle_cache().await;
        match bundles {
            Ok(bundles) => {
                for bundle in bundles {
                    match self.simulate_bundle(&bundle.bundle).await {
                        Ok(()) => in_progress.ingest_bundle(bundle.clone()),
                        Err(e) => error!(error = %e, id = ?bundle.id, "bundle simulation failed"),
                    }
                }
            }
            Err(e) => {
                error!(error = %e, "error polling bundles");
            }
        }
    }

    /// Simulates a Zenith bundle against the rollup state
    async fn simulate_bundle(&mut self, bundle: &SignetEthBundle) -> eyre::Result<()> {
        // TODO: Simulate bundles with the Simulation Engine
        // [ENG-672](https://linear.app/initiates/issue/ENG-672/add-support-for-bundles)
        debug!(hash = ?bundle.bundle.bundle_hash(), block_number = ?bundle.block_number(), "bundle simulations is not implemented yet - skipping simulation");
        Ok(())
    }

    async fn filter_transactions(&self, in_progress: &mut InProgressBlock) {
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
        trace!(confirmed = confirmed_transactions.len(), "found confirmed transactions");

        // remove already-confirmed transactions
        for transaction in confirmed_transactions {
            in_progress.remove_tx(&transaction);
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
    pub fn spawn(mut self, outbound: mpsc::UnboundedSender<InProgressBlock>) -> JoinHandle<()> {
        tokio::spawn(
            async move {
                loop {
                    // sleep the buffer time
                    tokio::time::sleep(Duration::from_secs(self.secs_to_next_target())).await;
                    info!("beginning block build cycle");

                    // Build a block
                    let mut in_progress = InProgressBlock::default();
                    self.get_transactions(&mut in_progress).await;
                    self.get_bundles(&mut in_progress).await;

                    // Filter confirmed transactions from the block
                    self.filter_transactions(&mut in_progress).await;

                    // submit the block if it has transactions
                    if !in_progress.is_empty() {
                        debug!(txns = in_progress.len(), "sending block to submit task");
                        let in_progress_block = std::mem::take(&mut in_progress);
                        if outbound.send(in_progress_block).is_err() {
                            error!("downstream task gone");
                            break;
                        }
                    } else {
                        debug!("no transactions, skipping block submission");
                    }
                }
            }
            .in_current_span(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;
    use alloy::{
        eips::eip2718::Encodable2718,
        network::{EthereumWallet, TransactionBuilder},
        rpc::types::{TransactionRequest, mev::EthSendBundle},
        signers::local::PrivateKeySigner,
    };
    use signet_bundle::SignetEthBundle;

    /// Create a mock bundle for testing with a single transaction
    async fn create_mock_bundle(wallet: &EthereumWallet) -> Bundle {
        let tx = TransactionRequest::default()
            .to(Address::ZERO)
            .from(wallet.default_signer().address())
            .nonce(1)
            .max_fee_per_gas(2)
            .max_priority_fee_per_gas(3)
            .gas_limit(4)
            .build(wallet)
            .await
            .unwrap()
            .encoded_2718();

        let eth_bundle = EthSendBundle {
            txs: vec![tx.into()],
            block_number: 1,
            min_timestamp: Some(u64::MIN),
            max_timestamp: Some(u64::MAX),
            reverting_tx_hashes: vec![],
            replacement_uuid: Some("replacement_uuid".to_owned()),
        };

        let zenith_bundle = SignetEthBundle { bundle: eth_bundle, host_fills: None };

        Bundle { id: "mock_bundle".to_owned(), bundle: zenith_bundle }
    }

    #[tokio::test]
    async fn test_ingest_bundle() {
        // Setup random creds
        let signer = PrivateKeySigner::random();
        let wallet = EthereumWallet::from(signer);

        // Create an empty InProgressBlock and bundle
        let mut in_progress_block = InProgressBlock::new();
        let bundle = create_mock_bundle(&wallet).await;

        // Save previous hash for comparison
        let prev_hash = in_progress_block.contents_hash();

        // Ingest the bundle
        in_progress_block.ingest_bundle(bundle);

        // Assert hash is changed after ingest
        assert_ne!(prev_hash, in_progress_block.contents_hash(), "Bundle should change block hash");

        // Assert that the transaction was persisted into block
        assert_eq!(in_progress_block.len(), 1, "Bundle should be persisted");

        // Assert that the block is properly sealed
        let raw_encoding = in_progress_block.encode_raw();
        assert!(!raw_encoding.is_empty(), "Raw encoding should not be empty");
    }
}
