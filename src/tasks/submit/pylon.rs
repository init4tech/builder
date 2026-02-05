//! Pylon Task handles submitting blob sidecars to the Pylon blob server
//! for data availability.

use crate::config::PylonClient;
use alloy::{eips::eip7594::BlobTransactionSidecarEip7594, primitives::TxHash};
use init4_bin_base::deps::metrics::counter;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{debug, error};

/// Task that submits blob sidecars to the Pylon blob server.
#[derive(Debug)]
pub struct PylonTask {
    /// Pylon client for posting sidecars.
    client: PylonClient,
}

impl PylonTask {
    /// Create a new PylonTask with the given client.
    pub const fn new(client: PylonClient) -> Self {
        Self { client }
    }

    /// Main task loop that processes sidecar submissions to Pylon.
    ///
    /// Receives `(TxHash, BlobTransactionSidecarEip7594)` tuples from the
    /// inbound channel and submits them to the Pylon blob server.
    async fn task_future(
        self,
        mut inbound: mpsc::UnboundedReceiver<(TxHash, BlobTransactionSidecarEip7594)>,
    ) {
        debug!("starting pylon task");

        loop {
            let Some((tx_hash, sidecar)) = inbound.recv().await else {
                debug!("upstream task gone - exiting pylon task");
                break;
            };

            let client = self.client.clone();

            tokio::spawn(async move {
                match client.post_sidecar(tx_hash, sidecar).await {
                    Ok(()) => {
                        counter!("signet.builder.pylon.posted").increment(1);
                        debug!(%tx_hash, "posted sidecar to pylon");
                    }
                    Err(err) => {
                        counter!("signet.builder.pylon.failures").increment(1);
                        error!(%tx_hash, %err, "pylon submission failed");
                    }
                }
            });
        }
    }

    /// Spawns the Pylon task in a new Tokio task.
    ///
    /// Returns a sender for submitting sidecars and a join handle for the task.
    pub fn spawn(
        self,
    ) -> (mpsc::UnboundedSender<(TxHash, BlobTransactionSidecarEip7594)>, JoinHandle<()>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let handle = tokio::spawn(self.task_future(receiver));
        (sender, handle)
    }
}
