use crate::{
    config::{BuilderConfig, HostProvider},
    quincey::{Quincey, QuinceyError},
    utils,
};
use alloy::{
    consensus::{Header, SimpleCoder},
    eips::eip7594::BlobTransactionSidecarEip7594,
    network::{TransactionBuilder, TransactionBuilder7594},
    primitives::{B256, Bytes, U256},
    providers::{Provider, WalletProvider},
    rpc::types::TransactionRequest,
    sol_types::SolCall,
};
use futures_util::FutureExt;
use init4_bin_base::deps::metrics::counter;
use signet_sim::BuiltBlock;
use signet_types::{SignRequest, SignResponse};
use signet_zenith::Zenith;
use tokio::try_join;
use tracing::{Instrument, debug, error, instrument, warn};

/// Preparation logic for transactions issued to the host chain by the [`FlashbotsTask`].
///
/// [`FlashbotsTask`]: crate::tasks::submit::FlashbotsTask
#[derive(Debug, Clone)]
pub struct SubmitPrep<'a> {
    // The block we are preparing a transaction for
    block: &'a BuiltBlock,

    // Info we need to prepare the transaction
    provider: HostProvider,
    quincey: Quincey,
    config: BuilderConfig,

    // Memoized quincey request and response
    sig_request: std::sync::OnceLock<SignRequest>,
    quincey_resp: tokio::sync::OnceCell<SignResponse>,
}

impl<'a> SubmitPrep<'a> {
    /// Create a new `SubmitPrep` instance.
    pub fn new(
        block: &'a BuiltBlock,
        provider: HostProvider,
        quincey: Quincey,
        config: BuilderConfig,
    ) -> Self {
        Self {
            block,
            sig_request: Default::default(),
            quincey_resp: Default::default(),
            provider,
            quincey,
            config,
        }
    }

    /// Construct a quincey signature request for the block.
    fn sig_request(&self) -> &SignRequest {
        self.sig_request.get_or_init(|| {
            let host_block_number =
                self.config.constants.rollup_block_to_host_block_num(self.block.block_number());

            SignRequest {
                host_block_number: U256::from(host_block_number),
                host_chain_id: U256::from(self.config.constants.host_chain_id()),
                ru_chain_id: U256::from(self.config.constants.ru_chain_id()),
                gas_limit: U256::from(self.config.rollup_block_gas_limit),
                ru_reward_address: self.config.builder_rewards_address,
                contents: *self.block.contents_hash(),
            }
        })
    }

    /// Get the quincey signature response for the block.
    async fn quincey_resp(&self) -> eyre::Result<&SignResponse> {
        self.quincey_resp
            .get_or_try_init(|| {
                async {
                    let sig_request = self.sig_request();
                    self.quincey
                        .get_signature(sig_request)
                        .await
                        .inspect(|_| counter!("signet.builder.quincey_signatures").increment(1))
                        .inspect_err(|err| {
                            counter!("signet.builder.quincey_signature_failures").increment(1);
                            if let QuinceyError::NotOurSlot = err {
                                warn!("Quincey indicated not our slot to sign");
                            } else {
                                error!(%err, "Error obtaining signature from Quincey");
                            }
                        })
                        .map_err(Into::into)
                }
                .in_current_span()
            })
            .await
    }

    /// Get the signature components from the response.
    async fn quincey_signature(&self) -> eyre::Result<(u8, B256, B256)> {
        self.quincey_resp().await.map(|resp| &resp.sig).map(utils::extract_signature_components)
    }

    /// Encodes the rollup block into an EIP-7594 sidecar.
    #[instrument(skip(self), level = "debug")]
    async fn build_sidecar(&self) -> eyre::Result<BlobTransactionSidecarEip7594> {
        self.block.encode_blob::<SimpleCoder>().build_7594().map_err(Into::into)
    }

    /// Build a signature and header input for the host chain transaction.
    async fn build_input(&self) -> eyre::Result<Bytes> {
        let (v, r, s) = self.quincey_signature().await?;

        let header = Zenith::BlockHeader {
            rollupChainId: U256::from(self.config.constants.ru_chain_id()),
            hostBlockNumber: self.sig_request().host_block_number,
            gasLimit: self.sig_request().gas_limit,
            rewardAddress: self.sig_request().ru_reward_address,
            blockDataHash: *self.block.contents_hash(),
        };
        let call = Zenith::submitBlockCall { header, v, r, s, _4: Bytes::new() };

        Ok(call.abi_encode().into())
    }

    /// Create a new transaction request for the host chain.
    async fn new_tx_request(&self) -> eyre::Result<TransactionRequest> {
        let nonce_fut = self
            .provider
            .get_transaction_count(self.provider.default_signer_address())
            .into_future()
            .map(|res| res.map_err(Into::into));

        let (nonce, sidecar, input) =
            try_join!(nonce_fut, self.build_sidecar(), self.build_input())?;

        let tx = TransactionRequest::default()
            .with_blob_sidecar(sidecar)
            .with_input(input)
            .with_to(self.config.constants.host_zenith())
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
        utils::populate_initial_gas(&mut req, prev_host);
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

    /// Consume the `Bumpable`, returning the inner transaction request.
    pub fn into_request(self) -> TransactionRequest {
        self.req
    }
}
