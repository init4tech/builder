use crate::{signer::LocalOrAws, tasks::oauth::SharedToken};
use alloy::signers::Signer;
use eyre::bail;
use init4_bin_base::deps::tracing::{self, debug, info, instrument, trace};
use oauth2::TokenResponse;
use reqwest::Client;
use signet_types::{SignRequest, SignResponse};

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

        let Some(token) = token.read() else { bail!("no token available") };

        let resp: reqwest::Response = client
            .post(url.clone())
            .json(sig_request)
            .bearer_auth(token.access_token().secret())
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
}
