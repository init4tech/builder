//! A generic Flashbots bundle API wrapper.
use alloy::network::{Ethereum, Network};
use alloy::primitives::Address;
use alloy::providers::{
    Provider, SendableTx,
    fillers::{FillerControlFlow, TxFiller},
};
use alloy::rpc::types::eth::TransactionRequest;
use alloy::rpc::types::mev::{EthBundleHash, MevSendBundle, ProtocolVersion};
use alloy_transport::TransportResult;
use eyre::Context as _;
use std::ops::Deref;

use crate::tasks::block::sim::SimResult;
use signet_types::SignedFill;

/// A wrapper over a `Provider` that adds Flashbots MEV bundle helpers.
#[derive(Debug, Clone)]
pub struct FlashbotsProvider<P> {
    inner: P,
    /// The base URL for the Flashbots API.
    pub relay_url: url::Url,
}

impl<P: Provider<Ethereum>> FlashbotsProvider<P> {
    /// Wraps a provider with the URL and returns a new `FlashbotsProvider`.
    pub fn new(inner: P, relay_url: url::Url) -> Self {
        Self { inner, relay_url }
    }

    /// Consume self and return the inner provider.
    pub fn into_inner(self) -> P {
        self.inner
    }

    /// Borrow the inner provider.
    pub const fn inner(&self) -> &P {
        &self.inner
    }
}

impl<P> Deref for FlashbotsProvider<P> {
    type Target = P;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<P> FlashbotsProvider<P>
where
    P: Provider<Ethereum> + Clone + Send + Sync + 'static,
{
    /// Convert a SignedFill to a TransactionRequest calling the Orders contract.
    ///
    /// This prepares the calldata for RollupOrders::fillPermit2(outputs, permit2) and sets
    /// `to` to the given Orders contract address. The returned request is unsigned.
    pub fn fill_to_tx_request(fill: &SignedFill, orders_contract: Address) -> TransactionRequest {
        fill.to_fill_tx(orders_contract)
    }

    /// Construct a new empty bundle template for the given block number.
    pub fn empty_bundle(&self, target_block: u64) -> MevSendBundle {
        MevSendBundle::new(target_block, Some(target_block), ProtocolVersion::V0_1, vec![])
    }

    /// Prepares a bundle transaction from the simulation result.
    pub fn prepare_bundle(&self, sim_result: &SimResult, target_block: u64) -> MevSendBundle {
        let bundle_body = Vec::new();

        // Populate the bundle body with the simulation result.

        // TODO: Push host fills into the Flashbots bundle body.
        let _host_fills = sim_result.block.host_fills();
        // _host_fills.iter().map(|f| f.to_fill_tx(todo!()));

        // TODO: Add the rollup block blob transaction to the Flashbots bundle body.
        // let blob_tx = ...;
        let _ = &sim_result; // keep param used until wired

        // Create the bundle from the target block and bundle body
        MevSendBundle::new(target_block, Some(target_block), ProtocolVersion::V0_1, bundle_body)
    }

    /// Submit the prepared Flashbots bundle to the relay via `mev_sendBundle`.
    pub async fn send_bundle(&self, bundle: MevSendBundle) -> eyre::Result<EthBundleHash> {
        // NOTE: The Flashbots relay expects a single parameter which is the bundle object.
        // Alloy's `raw_request` accepts any serializable params; wrapping in a 1-tuple is fine.
        let hash: EthBundleHash = self
            .inner
            .raw_request("mev_sendBundle".into(), (bundle,))
            .await
            .wrap_err("flashbots mev_sendBundle RPC failed")?;
        Ok(hash)
    }

    /// Simulate a bundle via `mev_simBundle`.
    pub async fn simulate_bundle(&self, bundle: &MevSendBundle) -> eyre::Result<()> {
        // We ignore the response (likely a JSON object with sim traces) for now and just ensure success.
        let _resp: serde_json::Value = self
            .inner
            .raw_request("mev_simBundle".into(), (bundle.clone(),))
            .await
            .wrap_err("flashbots mev_simBundle RPC failed")?;
        Ok(())
    }

    /// Check the status of a previously submitted bundle.
    pub async fn bundle_status(&self, _hash: EthBundleHash) -> eyre::Result<()> {
        eyre::bail!("FlashbotsProvider::bundle_status unimplemented")
    }
}

impl<N, P> TxFiller<N> for FlashbotsProvider<P>
where
    N: Network,
    P: TxFiller<N> + Provider<N> + Clone + Send + Sync + core::fmt::Debug + 'static,
{
    type Fillable = <P as TxFiller<N>>::Fillable;

    fn status(&self, tx: &N::TransactionRequest) -> FillerControlFlow {
        TxFiller::<N>::status(&self.inner, tx)
    }

    fn fill_sync(&self, tx: &mut SendableTx<N>) {
        TxFiller::<N>::fill_sync(&self.inner, tx)
    }

    fn prepare<Prov: Provider<N>>(
        &self,
        provider: &Prov,
        tx: &N::TransactionRequest,
    ) -> impl core::future::Future<Output = TransportResult<Self::Fillable>> + Send {
        TxFiller::<N>::prepare(&self.inner, provider, tx)
    }

    fn fill(
        &self,
        fillable: Self::Fillable,
        tx: SendableTx<N>,
    ) -> impl core::future::Future<Output = TransportResult<SendableTx<N>>> + Send {
        TxFiller::<N>::fill(&self.inner, fillable, tx)
    }
}
