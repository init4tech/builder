use alloy::{
    primitives::{Address, B256, U256},
    signers::Signer,
};
use eyre::bail;
use init4_bin_base::{perms::SharedToken, utils::signer::LocalOrAws};
use reqwest::Client;
use signet_types::{SignRequest, SignResponse};
use tracing::{debug, info, instrument, trace};

/// A quincey client for making requests to the Quincey API.
#[derive(Debug, Clone)]
pub enum Quincey {
    /// A remote quincey, this is used for production environments.
    /// The client will access the Quincey API over HTTP(S) via OAuth.
    Remote {
        /// The remote client.
        client: Client,
        /// The base URL for the remote API.
        url: reqwest::Url,
        /// OAuth shared token.
        token: SharedToken,
    },
    /// An owned quincey, either local or AWS. This is used primarily for
    /// testing and development environments. The client will simulate the
    /// Quincey API using a local or AWS KMS key.
    Owned(LocalOrAws),
}

impl Quincey {
    /// Creates a new Quincey client from the provided URL and token.
    pub const fn new_remote(client: Client, url: reqwest::Url, token: SharedToken) -> Self {
        Self::Remote { client, url, token }
    }

    /// Creates a new Quincey client for making requests to the Quincey API.
    pub const fn new_owned(client: LocalOrAws) -> Self {
        Self::Owned(client)
    }

    /// Returns `true` if the signer is local.
    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Owned(_))
    }

    /// Returns `true` if the signer is remote.
    pub const fn is_remote(&self) -> bool {
        matches!(self, Self::Remote { .. })
    }

    async fn sup_owned(&self, sig_request: &SignRequest) -> eyre::Result<SignResponse> {
        let Self::Owned(signer) = &self else { eyre::bail!("not an owned client") };

        info!("signing with owned quincey");
        signer
            .sign_hash(&sig_request.signing_hash())
            .await
            .map_err(Into::into)
            .map(|sig| SignResponse { sig, req: *sig_request })
    }

    async fn sup_remote(&self, sig_request: &SignRequest) -> eyre::Result<SignResponse> {
        let Self::Remote { client, url, token } = &self else { bail!("not a remote client") };

        let token =
            token.secret().await.map_err(|e| eyre::eyre!("failed to retrieve token: {e}"))?;

        let resp: reqwest::Response = client
            .post(url.clone())
            .json(sig_request)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?;

        let body = resp.bytes().await?;

        debug!(bytes = body.len(), "retrieved response body");
        trace!(body = %String::from_utf8_lossy(&body), "response body");

        serde_json::from_slice(&body).map_err(Into::into)
    }

    /// Get a signature for the provided request, by either using the owned
    /// or remote client.
    #[instrument(skip(self))]
    pub async fn get_signature(&self, sig_request: &SignRequest) -> eyre::Result<SignResponse> {
        match self {
            Self::Owned(_) => self.sup_owned(sig_request).await,
            Self::Remote { .. } => self.sup_remote(sig_request).await,
        }
    }

    /// Perform a preflight check to ensure that the Quincey service will
    /// be able to sign a request with the provided parameters at this
    /// point in time.
    #[instrument(skip(self))]
    pub async fn preflight_check(&self, host_block_number: u64) -> eyre::Result<()> {
        if self.is_local() {
            return Ok(());
        }
        let constants = crate::constants();
        let req = SignRequest {
            host_block_number: U256::from(host_block_number),
            host_chain_id: U256::from(constants.host_chain_id()),
            ru_chain_id: U256::from(constants.ru_chain_id()),
            gas_limit: U256::ZERO,
            ru_reward_address: Address::ZERO,
            contents: B256::ZERO,
        };
        self.sup_remote(&req).await.map(|_| ())
    }
}
