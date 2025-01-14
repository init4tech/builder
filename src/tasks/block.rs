use super::bundler::{Bundle, BundlePoller};
use super::oauth::Authenticator;
use super::tx_poller::TxPoller;
use crate::config::{BuilderConfig, WalletlessProvider};
use alloy::{
    consensus::{SidecarBuilder, SidecarCoder, TxEnvelope},
    eips::eip2718::Decodable2718,
    primitives::{keccak256, Bytes, FixedBytes, B256},
    providers::Provider as _,
    rpc::types::TransactionRequest,
};
use alloy_rlp::Buf;
use eyre::{bail, eyre};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{sync::OnceLock, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{error, Instrument};
use zenith_types::{encode_txns, Alloy2718Coder, ZenithEthBundle};

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
        tracing::trace!(hash = %tx.tx_hash(), "ingesting tx");
        self.unseal();
        self.transactions.push(tx.clone());
    }

    /// Remove a transaction from the in-progress block.
    pub fn remove_tx(&mut self, tx: &TxEnvelope) {
        tracing::trace!(hash = %tx.tx_hash(), "removing tx");
        self.unseal();
        self.transactions.retain(|t| t.tx_hash() != tx.tx_hash());
    }

    /// Ingest a bundle into the in-progress block.
    /// Ignores Signed Orders for now.
    pub fn ingest_bundle(&mut self, bundle: Bundle) {
        tracing::trace!(bundle = %bundle.id, "ingesting bundle");

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

/// BlockBuilder is a task that periodically builds a block then sends it for signing and submission.
pub struct BlockBuilder {
    pub config: BuilderConfig,
    pub ru_provider: WalletlessProvider,
    pub tx_poller: TxPoller,
    pub bundle_poller: BundlePoller,
}

impl BlockBuilder {
    // create a new block builder with the given config.
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

    /// Fetches transactions from the cache and ingests them into the in progress block
    async fn get_transactions(&mut self, in_progress: &mut InProgressBlock) {
        tracing::trace!("query transactions from cache");
        let txns = self.tx_poller.check_tx_cache().await;
        match txns {
            Ok(txns) => {
                tracing::trace!("got transactions response");
                for txn in txns.into_iter() {
                    in_progress.ingest_tx(&txn);
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "error polling transactions");
            }
        }
    }

    /// Fetches bundles from the cache and ingests them into the in progress block
    async fn get_bundles(
        &mut self,
        ru_provider: &WalletlessProvider,
        in_progress: &mut InProgressBlock,
    ) {
        // Authenticate before fetching to ensure access to a valid token
        if let Err(err) = self.bundle_poller.authenticator.authenticate().await {
            tracing::error!(err = %err, "bundle fetcher failed to authenticate");
            return;
        }

        tracing::trace!("query bundles from cache");
        let bundles = self.bundle_poller.check_bundle_cache().await;
        // OPTIMIZE: Sort bundles received from cache
        match bundles {
            Ok(bundles) => {
                for bundle in bundles {
                    let result = self.simulate_bundle(&bundle.bundle, ru_provider).await;
                    if result.is_ok() {
                        in_progress.ingest_bundle(bundle.clone());
                    }
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "error polling bundles");
            }
        }
        self.bundle_poller.evict();
    }

    /// Simulates a Flashbots-style `ZenithEthBundle`, simualating each transaction in its bundle
    /// by calling it against the host provider at the current height against default storage (no state overrides)
    /// and failing the whole bundle if any transaction not listed in the reverts list fails that call.
    async fn simulate_bundle(
        &mut self,
        bundle: &ZenithEthBundle,
        ru_provider: &WalletlessProvider,
    ) -> eyre::Result<()> {
        tracing::info!("simulating bundle");

        let reverts = &bundle.bundle.reverting_tx_hashes;
        tracing::debug!(reverts = ?reverts, "processing bundle with reverts");

        for tx in &bundle.bundle.txs {
            let (tx_env, hash) = self.parse_from_bundle(tx)?;
            tracing::debug!(?hash, "tx_envelope parsed from bundle");

            // Simulate and check for reversion allowance
            match self.simulate_transaction(ru_provider, tx_env).await {
                Ok(_) => {
                    // Passed, log trace and continue
                    tracing::debug!(tx = %hash, "tx passed simulation");
                    continue;
                }
                Err(sim_err) => {
                    // Failed, only continfue if tx is marked in revert list
                    tracing::debug!(?sim_err, "tx failed simulation");
                    if reverts.contains(&hash) {
                        continue;
                    } else {
                        bail!("tx {hash} failed simulation but was not marked as allowed to revert")
                    }
                }
            }
        }
        Ok(())
    }

    /// Simulates a rollup transaction by calling it on the ru provider at the current height with the current state.
    async fn simulate_transaction(
        &self,
        ru_provider: &WalletlessProvider,
        tx_env: TxEnvelope,
    ) -> eyre::Result<()> {
        let tx = TransactionRequest::from_transaction(tx_env);
        ru_provider.call(&tx).await?;
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
        tracing::trace!(confirmed = confirmed_transactions.len(), "found confirmed transactions");

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

    /// Parses bytes into a transaction envelope that is compatible with Flashbots-style bundles
    fn parse_from_bundle(&self, tx: &Bytes) -> Result<(TxEnvelope, FixedBytes<32>), eyre::Error> {
        let tx_env = TxEnvelope::decode_2718(&mut tx.chunk())?;
        let hash = tx_env.tx_hash().to_owned();
        tracing::debug!(hash = %hash, "decoded bundle tx");
        if tx_env.is_eip4844() {
            tracing::error!("eip-4844 disallowed");
            return Err(eyre!("EIP-4844 transactions are not allowed in bundles"));
        }
        Ok((tx_env, hash))
    }

    /// Spawn the block builder task, returning the inbound channel to it, and
    /// a handle to the running task.
    pub fn spawn(
        mut self,
        outbound: mpsc::UnboundedSender<InProgressBlock>,
        ru_provider: WalletlessProvider,
    ) -> JoinHandle<()> {
        tokio::spawn(
            async move {
                loop {
                    // sleep the buffer time
                    tokio::time::sleep(Duration::from_secs(self.secs_to_next_target())).await;
                    tracing::info!("beginning block build cycle");

                    // Build a block
                    let mut in_progress = InProgressBlock::default();
                    self.get_transactions(&mut in_progress).await;
                    self.get_bundles(&ru_provider, &mut in_progress).await;

                    // Filter confirmed transactions from the block
                    self.filter_transactions(&mut in_progress).await;

                    // submit the block if it has transactions
                    if !in_progress.is_empty() {
                        tracing::debug!(txns = in_progress.len(), "sending block to submit task");
                        let in_progress_block = std::mem::take(&mut in_progress);
                        if outbound.send(in_progress_block).is_err() {
                            tracing::error!("downstream task gone");
                            break;
                        }
                    } else {
                        tracing::debug!("no transactions, skipping block submission");
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
        rpc::types::{mev::EthSendBundle, TransactionRequest},
        signers::local::PrivateKeySigner,
    };
    use zenith_types::ZenithEthBundle;

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

        let zenith_bundle = ZenithEthBundle { bundle: eth_bundle, host_fills: None };

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
