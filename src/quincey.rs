use alloy::{
    primitives::{Address, B256, U256},
    signers::Signer,
};
use init4_bin_base::{perms::SharedToken, utils::signer::LocalOrAws};
use reqwest::Client;
use signet_types::{SignRequest, SignResponse};
use tracing::{info, instrument};

type Result<T> = core::result::Result<T, QuinceyError>;

/// Errors that can occur when interacting with the Quincey API.
#[derive(thiserror::Error, Debug)]
pub enum QuinceyError {
    /// Error indicating that the auth token is not available.
    #[error("Auth token not available")]
    Auth(#[from] tokio::sync::watch::error::RecvError),

    /// Error indicating that the request occurered during a slot is not
    /// assigned to this builder.
    #[error(
        "Quincey returned a 403 error, indicating that the request occurered during a slot is not assigned to this builder"
    )]
    NotOurSlot,

    /// Error contacting the remote quincey API.
    #[error("Error contacting quincey API: {0}")]
    Remote(reqwest::Error),

    /// Error with the owned signet.
    #[error("Error with owned signet: {0}")]
    Owned(#[from] eyre::Report),
}

impl From<reqwest::Error> for QuinceyError {
    fn from(err: reqwest::Error) -> Self {
        if err.status() == Some(reqwest::StatusCode::FORBIDDEN) {
            QuinceyError::NotOurSlot
        } else {
            QuinceyError::Remote(err)
        }
    }
}

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
        let Self::Owned(signer) = &self else { panic!("not an owned client") };

        info!("signing with owned quincey");
        signer
            .sign_hash(&sig_request.signing_hash())
            .await
            .map_err(Into::into)
            .map(|sig| SignResponse { sig, req: *sig_request })
    }

    #[instrument(skip_all)]
    async fn sup_remote(&self, sig_request: &SignRequest) -> Result<SignResponse> {
        let Self::Remote { client, url, token } = &self else { panic!("not a remote client") };

        let token = token.secret().await?;

        let resp = client.post(url.clone()).json(sig_request).bearer_auth(token).send().await?;

        resp.error_for_status()?.json::<SignResponse>().await.map_err(QuinceyError::Remote)
    }

    /// Get a signature for the provided request, by either using the owned
    /// or remote client.
    #[instrument(skip(self))]
    pub async fn get_signature(&self, sig_request: &SignRequest) -> Result<SignResponse> {
        match self {
            Self::Owned(_) => self.sup_owned(sig_request).await.map_err(Into::into),
            Self::Remote { .. } => self.sup_remote(sig_request).await,
        }
    }

    /// Perform a preflight check to ensure that the Quincey service will
    /// be able to sign a request with the provided parameters at this
    /// point in time.
    #[instrument(skip(self))]
    pub async fn preflight_check(&self, host_block_number: u64) -> Result<()> {
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
