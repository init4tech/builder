//! Flashbots Task receives simulated blocks from an upstream channel and
//! submits them to the Flashbots relay as bundles.
use crate::{
    config::{BuilderConfig, FlashbotsProvider, HostProvider, ZenithInstance},
    quincey::Quincey,
    tasks::{block::sim::SimResult, submit::SubmitPrep},
};
use alloy::{
    eips::Encodable2718,
    primitives::TxHash,
    providers::ext::MevApi,
    rpc::types::mev::{BundleItem, MevSendBundle, ProtocolVersion},
};
use eyre::OptionExt;
use init4_bin_base::utils::signer::LocalOrAws;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{Instrument, debug};

/// Handles construction, simulation, and submission of rollup blocks to the
/// Flashbots network.
#[derive(Debug)]
pub struct FlashbotsTask {
    /// Builder configuration for the task.
    config: BuilderConfig,
    /// Quincey instance for block signing.
    quincey: Quincey,
    /// Zenith instance.
    zenith: ZenithInstance<HostProvider>,
    /// Provides access to a Flashbots-compatible bundle API.
    flashbots: FlashbotsProvider,
    /// The key used to sign requests to the Flashbots relay.
    signer: LocalOrAws,
    /// Channel for sending hashes of outbound transactions.
    _outbound: mpsc::UnboundedSender<TxHash>,
}

impl FlashbotsTask {
    /// Returns a new `FlashbotsTask` instance that receives `SimResult` types from the given
    /// channel and handles their preparation, submission to the Flashbots network.
    pub async fn new(
        config: BuilderConfig,
        outbound: mpsc::UnboundedSender<TxHash>,
    ) -> eyre::Result<FlashbotsTask> {
        let (quincey, host_provider, flashbots, builder_key) = tokio::try_join!(
            config.connect_quincey(),
            config.connect_host_provider(),
            config.connect_flashbots(&config),
            config.connect_builder_signer()
        )?;

        let zenith = config.connect_zenith(host_provider);

        Ok(Self { config, quincey, zenith, flashbots, signer: builder_key, _outbound: outbound })
    }

    /// Returns a reference to the inner `HostProvider`
    pub fn host_provider(&self) -> HostProvider {
        self.zenith.provider().clone()
    }

    /// Returns a reference to the inner `FlashbotsProvider`
    pub const fn flashbots(&self) -> &FlashbotsProvider {
        &self.flashbots
    }

    /// Prepares a MEV bundle with the configured submit call
    pub async fn prepare(&self, sim_result: &SimResult) -> eyre::Result<MevSendBundle> {
        // This function is left for forwards compatibility when we want to add
        // different bundle preparation methods in the future.
        self.prepare_bundle_helper(sim_result).await
    }

    /// Prepares a BundleHelper call containing the rollup block and corresponding fills into a MEV bundle.
    async fn prepare_bundle_helper(&self, sim_result: &SimResult) -> eyre::Result<MevSendBundle> {
        let prep = SubmitPrep::new(
            &sim_result.block,
            self.host_provider(),
            self.quincey.clone(),
            self.config.clone(),
        );

        let tx = prep.prep_transaction(&sim_result.sim_env.prev_host).await?;

        let sendable = self.host_provider().fill(tx.into_request()).await?;

        let tx_bytes = sendable
            .as_envelope()
            .ok_or_eyre("failed to get envelope from filled tx")?
            .encoded_2718()
            .into();

        let bundle_body = sim_result
            .block
            .host_txns()
            .iter()
            .cloned()
            .chain(std::iter::once(tx_bytes))
            .map(|tx| BundleItem::Tx { tx, can_revert: false })
            .collect::<Vec<_>>();

        // Only valid in the specific host block
        Ok(MevSendBundle::new(
            sim_result.host_block_number(),
            Some(sim_result.host_block_number()),
            ProtocolVersion::V0_1,
            bundle_body,
        ))
    }

    /// Task future that runs the Flashbots submission loop.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<SimResult>) {
        debug!("starting flashbots task");

        loop {
            // Wait for a sim result to come in
            let Some(sim_result) = inbound.recv().await else {
                debug!("upstream task gone - exiting flashbots task");
                break;
            };
            let span = sim_result.span();
            span_debug!(
                span,
                host_block_number = sim_result.host_block_number(),
                "received sim result"
            );

            // Prepare a MEV bundle with the configured call type from the sim result
            let bundle = match self.prepare(&sim_result).instrument(span.clone()).await {
                Ok(b) => b,
                Err(e) => {
                    span_error!(span, %e, "failed to prepare MEV bundle");
                    continue;
                }
            };

            // Send the bundle to Flashbots
            let response = self
                .flashbots()
                .send_mev_bundle(bundle.clone())
                .with_auth(self.signer.clone())
                .await;

            match response {
                Ok(Some(hash)) => {
                    span_debug!(
                        span,
                        hash = hash.bundle_hash.to_string(),
                        "received bundle hash after submitted to flashbots"
                    );
                }
                Ok(None) => {
                    span_debug!(span, "received no bundle hash after submitted to flashbots");
                }
                Err(err) => {
                    span_error!(span, %err, "MEV bundle submission failed - error returned");
                }
            }
        }
    }

    /// Spawns the Flashbots task that handles incoming `SimResult`s.
    pub fn spawn(self) -> (mpsc::UnboundedSender<SimResult>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel::<SimResult>();
        let handle = tokio::spawn(self.task_future(inbound));
        (sender, handle)
    }
}
