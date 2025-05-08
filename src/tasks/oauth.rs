//! Service responsible for authenticating with the cache with Oauth tokens.
//! This authenticator periodically fetches a new token every set amount of seconds.
use crate::config::BuilderConfig;
use init4_bin_base::deps::tracing::{error, info};
use oauth2::{
    AuthUrl, ClientId, ClientSecret, EmptyExtraTokenFields, StandardTokenResponse, TokenUrl,
    basic::{BasicClient, BasicTokenType},
    reqwest::async_http_client,
};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

type Token = StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>;

/// A shared token that can be read and written to by multiple threads.
#[derive(Debug, Clone, Default)]
pub struct SharedToken(Arc<Mutex<Option<Token>>>);

impl SharedToken {
    /// Read the token from the shared token.
    pub fn read(&self) -> Option<Token> {
        self.0.lock().unwrap().clone()
    }

    /// Write a new token to the shared token.
    pub fn write(&self, token: Token) {
        let mut lock = self.0.lock().unwrap();
        *lock = Some(token);
    }

    /// Check if the token is authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

/// A self-refreshing, periodically fetching authenticator for the block
/// builder. This task periodically fetches a new token, and stores it in a
/// [`SharedToken`].
#[derive(Debug)]
pub struct Authenticator {
    /// Configuration
    pub config: BuilderConfig,
    client: BasicClient,
    token: SharedToken,
}

impl Authenticator {
    /// Creates a new Authenticator from the provided builder config.
    pub fn new(config: &BuilderConfig) -> eyre::Result<Self> {
        let client = BasicClient::new(
            ClientId::new(config.oauth_client_id.clone()),
            Some(ClientSecret::new(config.oauth_client_secret.clone())),
            AuthUrl::new(config.oauth_authenticate_url.clone())?,
            Some(TokenUrl::new(config.oauth_token_url.clone())?),
        );

        Ok(Self { config: config.clone(), client, token: Default::default() })
    }

    /// Requests a new authentication token and, if successful, sets it to as the token
    pub async fn authenticate(&self) -> eyre::Result<()> {
        let token = self.fetch_oauth_token().await?;
        self.set_token(token);
        Ok(())
    }

    /// Returns true if there is Some token set
    pub fn is_authenticated(&self) -> bool {
        self.token.is_authenticated()
    }

    /// Sets the Authenticator's token to the provided value
    fn set_token(&self, token: StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>) {
        self.token.write(token);
    }

    /// Returns the currently set token
    pub fn token(&self) -> SharedToken {
        self.token.clone()
    }

    /// Fetches an oauth token
    pub async fn fetch_oauth_token(&self) -> eyre::Result<Token> {
        let token_result =
            self.client.exchange_client_credentials().request_async(async_http_client).await?;

        Ok(token_result)
    }

    /// Spawns a task that periodically fetches a new token every 300 seconds.
    pub fn spawn(self) -> JoinHandle<()> {
        let interval = self.config.oauth_token_refresh_interval;

        let handle: JoinHandle<()> = tokio::spawn(async move {
            loop {
                info!("Refreshing oauth token");
                match self.authenticate().await {
                    Ok(_) => {
                        info!("Successfully refreshed oauth token");
                    }
                    Err(e) => {
                        error!(%e, "Failed to refresh oauth token");
                    }
                };
                let _sleep = tokio::time::sleep(tokio::time::Duration::from_secs(interval)).await;
            }
        });

        handle
    }
}
