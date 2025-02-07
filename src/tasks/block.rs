use super::bundler::Bundle;
use alloy::{
    consensus::{SidecarBuilder, SidecarCoder, TxEnvelope},
    eips::eip2718::Decodable2718,
    primitives::{keccak256, Bytes, B256},
    rlp::Buf,
};
use std::sync::OnceLock;
use tracing::{error, trace};
use zenith_types::{encode_txns, Alloy2718Coder};

/// A block in progress.
#[derive(Debug, Default, Clone)]
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
