//! Service responsible for authenticating with the cache with Oauth tokens.
//! This authenticator periodically fetches a new token every set amount of seconds.
use std::sync::Arc;

use crate::config::BuilderConfig;
use oauth2::{
    basic::{BasicClient, BasicTokenType},
    reqwest::http_client,
    AuthUrl, ClientId, ClientSecret, EmptyExtraTokenFields, StandardTokenResponse, TokenUrl,
};
use tokio::{sync::RwLock, task::JoinHandle};

const OAUTH_AUDIENCE_CLAIM: &str = "audience";

type Token = StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>;

/// A self-refreshing, periodically fetching authenticator for the block builder.
/// It is architected as a shareable struct that can be used across all the multiple builder tasks.
/// It fetches a new token every set amount of seconds, configured through the general builder config.
/// Readers are guaranteed to not read stale tokens as the [RwLock] guarantees that write tasks (refreshing the token) will claim priority over read access.
#[derive(Debug, Clone)]
pub struct Authenticator {
    pub config: BuilderConfig,
    inner: Arc<RwLock<AuthenticatorInner>>,
}

/// Inner state of the Authenticator.
/// Contains the token that is being used for authentication.
#[derive(Debug)]
pub struct AuthenticatorInner {
    pub token: Option<Token>,
}

impl Default for AuthenticatorInner {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthenticatorInner {
    pub fn new() -> Self {
        Self { token: None }
    }
}

impl Authenticator {
    /// Creates a new Authenticator from the provided builder config.
    pub fn new(config: &BuilderConfig) -> Self {
        Self { config: config.clone(), inner: Arc::new(RwLock::new(AuthenticatorInner::new())) }
    }

    /// Requests a new authentication token and, if successful, sets it to as the token
    pub async fn authenticate(&self) -> eyre::Result<()> {
        let token = self.fetch_oauth_token().await?;
        self.set_token(token).await;
        Ok(())
    }

    /// Returns true if there is Some token set
    pub async fn is_authenticated(&self) -> bool {
        let lock = self.inner.read().await;

        lock.token.is_some()
    }

    /// Sets the Authenticator's token to the provided value
    pub async fn set_token(
        &self,
        token: StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>,
    ) {
        let mut lock = self.inner.write().await;
        lock.token = Some(token);
    }

    /// Returns the currently set token
    pub async fn token(&self) -> Option<Token> {
        let lock = self.inner.read().await;
        lock.token.clone()
    }

    /// Fetches an oauth token
    pub async fn fetch_oauth_token(
        &self,
    ) -> eyre::Result<StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>> {
        let config = self.config.clone();

        let client = BasicClient::new(
            ClientId::new(config.oauth_client_id.clone()),
            Some(ClientSecret::new(config.oauth_client_secret.clone())),
            AuthUrl::new(config.oauth_authenticate_url.clone())?,
            Some(TokenUrl::new(config.oauth_token_url.clone())?),
        );

        let token_result = client
            .exchange_client_credentials()
            .add_extra_param(OAUTH_AUDIENCE_CLAIM, config.oauth_audience.clone())
            .request(http_client)?;

        Ok(token_result)
    }

    /// Spawns a task that periodically fetches a new token every 300 seconds.
    pub fn spawn(self) -> JoinHandle<()> {
        let interval = self.config.oauth_token_refresh_interval;

        let handle: JoinHandle<()> = tokio::spawn(async move {
            loop {
                tracing::info!("Refreshing oauth token");
                match self.authenticate().await {
                    Ok(_) => {
                        tracing::info!("Successfully refreshed oauth token");
                    }
                    Err(e) => {
                        tracing::error!(%e, "Failed to refresh oauth token");
                    }
                };
                let _sleep = tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
            }
        });

        handle
    }
}

mod tests {
    use crate::config::BuilderConfig;
    use alloy_primitives::Address;
    use eyre::Result;

    #[ignore = "integration test"]
    #[tokio::test]
    async fn test_authenticator() -> Result<()> {
        use super::*;
        use oauth2::TokenResponse;

        let config = setup_test_config()?;
        let auth = Authenticator::new(&config);
        let token = auth.fetch_oauth_token().await?;
        dbg!(&token);
        let token = auth.token().await.unwrap();
        println!("{:?}", token);
        assert!(!token.access_token().secret().is_empty());
        Ok(())
    }

    #[allow(dead_code)]
    pub fn setup_test_config() -> Result<BuilderConfig> {
        let config = BuilderConfig {
            host_chain_id: 17000,
            ru_chain_id: 17001,
            host_rpc_url: "http://rpc.holesky.signet.sh".into(),
            zenith_address: Address::default(),
            quincey_url: "http://localhost:8080".into(),
            builder_port: 8080,
            sequencer_key: None,
            builder_key: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            block_confirmation_buffer: 1,
            chain_offset: 0,
            target_slot_time: 1,
            builder_rewards_address: Address::default(),
            rollup_block_gas_limit: 100_000,
            tx_pool_url: "http://localhost:9000/".into(),
            tx_pool_cache_duration: 5,
            oauth_client_id: "some_client_id".into(),
            oauth_client_secret: "some_client_secret".into(),
            oauth_authenticate_url: "http://localhost:9000".into(),
            oauth_token_url: "http://localhost:9000".into(),
            oauth_audience: "https://transactions.holesky.signet.sh".into(),
            tx_broadcast_urls: vec!["http://localhost:9000".into()],
            oauth_token_refresh_interval: 300, // 5 minutes
        };
        Ok(config)
    }
}
