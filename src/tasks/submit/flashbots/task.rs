//! Flashbots Task receives simulated blocks from an upstream channel and
//! submits them to the Flashbots relay as bundles.
use crate::{
    quincey::Quincey,
    tasks::{block::sim::SimResult, submit::flashbots::FlashbotsProvider},
};
use alloy::{network::Ethereum, primitives::TxHash, providers::Provider};
use init4_bin_base::deps::tracing::{debug, error};
use signet_constants::SignetSystemConstants;
use tokio::{sync::mpsc, task::JoinHandle};

/// Handles construction, simulation, and submission of rollup blocks to the
/// Flashbots network.
#[derive(Debug)]
pub struct FlashbotsTask<P> {
    /// Builder configuration for the task.
    pub config: crate::config::BuilderConfig,
    /// System constants for the rollup and host chain.
    pub constants: SignetSystemConstants,
    /// Quincey instance for block signing.
    pub quincey: Quincey,
    /// Provider for interacting with  Flashbots API.
    pub provider: P,
    /// Channel for sending hashes of outbound transactions.
    pub outbound: mpsc::UnboundedSender<TxHash>,
}

impl<P> FlashbotsTask<P>
where
    P: Provider<Ethereum> + Clone + Send + Sync + 'static,
{
    /// Returns a new FlashbotsTask instance that receives `SimResult` types from the given
    /// channel and handles their preparation, submission to the Flashbots network.
    pub fn new(
        config: crate::config::BuilderConfig,
        constants: SignetSystemConstants,
        quincey: Quincey,
        provider: P,
        outbound: mpsc::UnboundedSender<TxHash>,
    ) -> FlashbotsTask<P> {
        Self { config, constants, quincey, provider, outbound }
    }

    /// Task future that runs the Flashbots submission loop.
    async fn task_future(self, mut inbound: mpsc::UnboundedReceiver<SimResult>) {
        debug!("starting flashbots task");

        // Wrap the underlying host provider with Flashbots helpers.
        let flashbots =
            FlashbotsProvider::new(self.provider, self.config.flashbots_endpoint.into());

        loop {
            let Some(sim_result) = inbound.recv().await else {
                debug!("upstream task gone - exiting flashbots task");
                break;
            };
            debug!(?sim_result.env.block_env.number, "simulation block received");

            let current_block: u64 = sim_result.env.block_env.number.to::<u64>();
            let target_block = current_block + 1;

            // TODO: populate and prepare bundle from `SimResult` once `BundleProvider` is wired.
            let bundle = flashbots.prepare_bundle(&sim_result, target_block);

            // simulate then send the bundle
            let sim_bundle_result = flashbots.simulate_bundle(&bundle).await;
            match sim_bundle_result {
                Ok(_) => {
                    debug!("bundle simulation successful, ready to send");
                    match flashbots.send_bundle(bundle).await {
                        Ok(bundle_hash) => {
                            debug!(?bundle_hash, "bundle successfully sent to Flashbots");
                        }
                        Err(err) => {
                            error!(?err, "failed to send bundle to Flashbots");
                            continue;
                        }
                    }
                }
                Err(err) => {
                    error!(?err, "bundle simulation failed");
                    continue;
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
