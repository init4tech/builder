//! Flashbots Task receives simulated blocks from an upstream channel and
//! submits them to the Flashbots relay as bundles.
use crate::{
    config::{BuilderConfig, HostProvider, ZenithInstance},
    quincey::Quincey,
    tasks::{block::sim::SimResult, submit::SubmitPrep},
};
use alloy::{
    eips::Encodable2718,
    primitives::TxHash,
    rpc::types::mev::{BundleItem, MevSendBundle, ProtocolVersion},
};
use eyre::OptionExt;
use init4_bin_base::{
    deps::tracing::{debug, error},
    utils::flashbots::Flashbots,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::Instrument;

/// Handles construction, simulation, and submission of rollup blocks to the
/// Flashbots network.
#[derive(Debug)]
pub struct FlashbotsTask {
    /// Builder configuration for the task.
    config: BuilderConfig,
    /// Flashbots RPC provider.
    flashbots: Flashbots,
    /// Quincey instance for block signing.
    quincey: Quincey,
    /// Zenith instance.
    zenith: ZenithInstance<HostProvider>,
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
        let (flashbots, quincey, host_provider) = tokio::try_join!(
            config.flashbots_provider(),
            config.connect_quincey(),
            config.connect_host_provider(),
        )?;

        let zenith = config.connect_zenith(host_provider);

        Ok(Self { config, flashbots, quincey, zenith, _outbound: outbound })
    }

    /// Returns a reference to the inner `HostProvider`
    pub fn host_provider(&self) -> HostProvider {
        self.zenith.provider().clone()
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

        let tx = prep.prep_transaction(&sim_result.sim_env.prev_header).await?;

        let sendable = self.host_provider().fill(tx.into_request()).await?;

        let tx_bytes = sendable
            .as_envelope()
            .ok_or_eyre("failed to get envelope from filled tx")?
            .encoded_2718()
            .into();

        // Only valid in the specific host block
        Ok(MevSendBundle::new(
            sim_result.host_block_number(),
            Some(sim_result.host_block_number()),
            ProtocolVersion::V0_1,
            vec![BundleItem::Tx { tx: tx_bytes, can_revert: false }],
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
            span_debug!(span, "simulation result received");

            // Prepare a MEV bundle with the configured call type from the sim result
            let Ok(bundle) = self.prepare(&sim_result).instrument(span.clone()).await else {
                span_debug!(span, "bundle preparation failed");
                continue;
            };

            // simulate the bundle against Flashbots then send the bundle
            if let Err(err) = self.flashbots.simulate_bundle(&bundle).instrument(span.clone()).await
            {
                span_debug!(span, %err, "bundle simulation failed");
                continue;
            }

            let _ = self
                .flashbots
                .send_bundle(&bundle)
                .instrument(span.clone())
                .await
                .inspect(|bundle_hash| {
                    span_debug!(
                        span,
                        bundle_hash = %bundle_hash.bundle_hash, "bundle sent to Flashbots"
                    );
                })
                .inspect_err(|err| {
                    span_error!(span, %err, "failed to send bundle to Flashbots");
                });
        }
    }

    /// Spawns the Flashbots task that handles incoming `SimResult`s.
    pub fn spawn(self) -> (mpsc::UnboundedSender<SimResult>, JoinHandle<()>) {
        let (sender, inbound) = mpsc::unbounded_channel::<SimResult>();
        let handle = tokio::spawn(self.task_future(inbound));
        (sender, handle)
    }
}
