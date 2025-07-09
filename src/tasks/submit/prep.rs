use crate::{
    config::{BuilderConfig, HostProvider},
    quincey::Quincey,
    utils,
};
use alloy::{
    consensus::{Header, SimpleCoder},
    network::{TransactionBuilder, TransactionBuilder4844},
    primitives::{B256, U256},
    providers::{Provider, WalletProvider},
    rpc::types::TransactionRequest,
    sol_types::SolCall,
};
use init4_bin_base::deps::tracing::debug;
use signet_constants::SignetSystemConstants;
use signet_sim::BuiltBlock;
use signet_types::{SignRequest, SignResponse};
use signet_zenith::BundleHelper;
use std::sync::OnceLock;
use tracing::Instrument;

/// Preparation logic for transactions issued to the host chain by the
/// [`SubmitTask`].
///
/// [`SubmitTask`]: crate::tasks::submit::SubmitTask
#[derive(Debug, Clone)]
pub struct SubmitPrep<'a> {
    // The block we are preparing a transaction for
    block: &'a BuiltBlock,

    // Info we need to prepare the transaction
    provider: HostProvider,
    quincey: Quincey,
    config: BuilderConfig,
    constants: SignetSystemConstants,

    // Memoized quincey request and response
    sig_request: OnceLock<SignRequest>,
    quincey_resp: OnceLock<SignResponse>,
}

impl<'a> SubmitPrep<'a> {
    /// Create a new `SubmitPrep` instance.
    pub fn new(
        block: &'a BuiltBlock,
        provider: HostProvider,
        quincey: Quincey,
        config: BuilderConfig,
        constants: SignetSystemConstants,
    ) -> Self {
        Self {
            block,
            sig_request: Default::default(),
            quincey_resp: Default::default(),
            provider,
            quincey,
            config,
            constants,
        }
    }

    /// Construct a quincey signature request for the block.
    fn sig_request(&self) -> &SignRequest {
        self.sig_request.get_or_init(|| {
            let host_block_number =
                self.constants.rollup_block_to_host_block_num(self.block.block_number());

            SignRequest {
                host_block_number: U256::from(host_block_number),
                host_chain_id: U256::from(self.config.host_chain_id),
                ru_chain_id: U256::from(self.config.ru_chain_id),
                gas_limit: U256::from(self.config.rollup_block_gas_limit),
                ru_reward_address: self.config.builder_rewards_address,
                contents: *self.block.contents_hash(),
            }
        })
    }

    /// Get the quincey signature response for the block.
    async fn quincey_resp(&self) -> eyre::Result<&SignResponse> {
        if let Some(resp) = self.quincey_resp.get() {
            return Ok(resp);
        }

        let sig = self.quincey.get_signature(self.sig_request()).await?;
        Ok(self.quincey_resp.get_or_init(|| sig))
    }

    /// Get the signature components from the response.
    async fn quincey_signature(&self) -> eyre::Result<(u8, B256, B256)> {
        self.quincey_resp().await.map(|resp| &resp.sig).map(utils::extract_signature_components)
    }

    /// Converts the fills in the block to a vector of `FillPermit2`.
    fn fills(&self) -> Vec<BundleHelper::FillPermit2> {
        utils::convert_fills(self.block)
    }

    /// Encodes the sidecar and then builds the 4844 blob transaction from the provided header and signature values.
    async fn build_blob_tx(&self) -> eyre::Result<TransactionRequest> {
        let (v, r, s) = self.quincey_signature().await?;

        // Build the block header
        let header = BundleHelper::BlockHeader {
            hostBlockNumber: self.sig_request().host_block_number,
            rollupChainId: U256::from(self.constants.ru_chain_id()),
            gasLimit: self.sig_request().gas_limit,
            rewardAddress: self.sig_request().ru_reward_address,
            blockDataHash: *self.block.contents_hash(),
        };
        debug!(?header.hostBlockNumber, "built rollup block header");

        let fills = self.fills();
        debug!(?fills, "prepared fills for blob tx");

        let data = BundleHelper::submitCall { fills, header, v, r, s }.abi_encode();

        let sidecar = self.block.encode_blob::<SimpleCoder>().build()?;

        Ok(TransactionRequest::default().with_blob_sidecar(sidecar).with_input(data))
    }

    async fn new_tx_request(&self) -> eyre::Result<TransactionRequest> {
        let nonce =
            self.provider.get_transaction_count(self.provider.default_signer_address()).await?;

        debug!(nonce, "assigned nonce");

        // Create a blob transaction with the blob header and signature values and return it
        let tx = self
            .build_blob_tx()
            .await?
            .with_to(self.config.builder_helper_address)
            .with_nonce(nonce);

        Ok(tx)
    }

    /// Prepares a transaction for submission to the host chain.
    pub async fn prep_transaction(self, prev_host: &Header) -> eyre::Result<Bumpable> {
        let req = self.new_tx_request().in_current_span().await?;
        Ok(Bumpable::new(req, prev_host))
    }
}

/// A fee-bumpable transaction request for the host chain.
#[derive(Debug, Clone)]
pub struct Bumpable {
    req: TransactionRequest,
    bumps: usize,
}

impl Bumpable {
    /// Instantiate a new `Bumpable` transaction request.
    pub fn new(mut req: TransactionRequest, prev_host: &Header) -> Self {
        crate::utils::populate_initial_gas(&mut req, prev_host);
        Self { req, bumps: 0 }
    }

    /// Get a reference to the inner transaction request.
    pub const fn req(&self) -> &TransactionRequest {
        &self.req
    }

    /// Get the current bump count.
    pub const fn bump_count(&self) -> usize {
        self.bumps
    }

    /// Bump the fees for the transaction request.
    pub fn bump(&mut self) {
        self.bumps += 1;

        // Bump max_priority fee per gas by 20%
        let mpfpg = self.req.max_priority_fee_per_gas.as_mut().expect("set on construction");
        let bump = *mpfpg / 5;
        *mpfpg += bump;

        // Increase max_fee_per_gas by the same amount as we increased mpfpg
        let mfpg = self.req.max_fee_per_gas.as_mut().expect("set on construction");
        *mfpg += bump;

        // Do not bump max_fee_per_blob_gas, as we require confirmation in a
        // specific block, and the blob base fee in the block is known.

        debug!(new_mpfpg = mpfpg, new_mfpg = mfpg, "Bumped fees",);
    }

    /// Bump the fees, and return a copy of the transaction request.
    pub fn bumped(&mut self) -> TransactionRequest {
        self.bump();
        self.req.clone()
    }
}
