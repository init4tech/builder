use crate::config::BuilderConfig;
use oauth2::{
    basic::{BasicClient, BasicTokenType},
    reqwest::http_client,
    AuthUrl, ClientId, ClientSecret, EmptyExtraTokenFields, StandardTokenResponse, TokenUrl,
};

const OAUTH_AUDIENCE_CLAIM: &str = "audience";

/// Holds a reference to the current oauth token and the builder config
#[derive(Debug, Clone)]
pub struct Authenticator {
    pub config: BuilderConfig,
    pub token: Option<StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>>,
}

impl Authenticator {
    /// Creates a new Authenticator from the provided builder config.
    pub async fn new(config: &BuilderConfig) -> eyre::Result<Self> {
        Ok(Self {
            config: config.clone(),
            token: None,
        })
    }

    /// Requests a new authentication token and, if successful, sets it to as the token
    pub async fn authenticate(&mut self) -> eyre::Result<()> {
        let token = self.fetch_oauth_token()?;
        dbg!(&token);
        // self.set_token(token).await;
        Ok(())
    }

    /// Returns true if there is Some token set
    pub async fn is_authenticated(&self) -> bool {
        // TODO: Consider checking if the token is still valid and fetching a new one if it's not
        self.token.is_some()
    }

    /// Sets the Authenticator's token to the provided value
    pub async fn set_token(
        &mut self,
        token: StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>,
    ) {
        self.token = Some(token);
    }

    /// Returns the currently set token
    pub async fn token(
        &self,
    ) -> eyre::Result<StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>> {
        self.token
            .as_ref()
            .ok_or(eyre::eyre!("no token set"))
            .cloned()
    }

    /// Fetches an oauth token and saves it to the Authenticator
    /// Does not set a new token, it only requests a new one
    pub fn fetch_oauth_token(
        &self,
    ) -> eyre::Result<StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>> {
        let client = BasicClient::new(
            ClientId::new(self.config.oauth_client_id.clone()),
            Some(ClientSecret::new(self.config.oauth_client_secret.clone())),
            AuthUrl::new(self.config.oauth_authenticate_url.clone())?,
            Some(TokenUrl::new(self.config.oauth_token_url.clone())?),
        );

        let token_result = client
            .exchange_client_credentials()
            .add_extra_param(OAUTH_AUDIENCE_CLAIM, self.config.oauth_audience.clone())
            .request(http_client)?;

        Ok(token_result)
    }
}

mod tests {
    use super::*;

    use crate::{config::BuilderConfig, tasks::block::BlockBuilder};
    use alloy_primitives::Address;
    use eyre::Result;

    #[tokio::test]
    async fn test_authenticator() -> Result<()> {
        let config = setup_test_builder()?.1;
        let auth = Authenticator::new(&config).await?;
        let token = auth.fetch_oauth_token()?;
        dbg!(&token);
        // let token = auth.token().await?;
        // println!("{:?}", token);
        // assert!(token.access_token().secret().len() > 0);
        Ok(())
    }

    pub fn setup_test_builder() -> Result<(BlockBuilder, BuilderConfig)> {
        let config = BuilderConfig {
            host_chain_id: 17000,
            ru_chain_id: 17001,
            host_rpc_url: "http://rpc.holesky.signet.sh".into(),
            zenith_address: Address::default(),
            quincey_url: "http://localhost:8080".into(),
            builder_port: 8080,
            sequencer_key: None,
            builder_key: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            incoming_transactions_buffer: 1,
            block_confirmation_buffer: 1,
            builder_rewards_address: Address::default(),
            rollup_block_gas_limit: 100_000,
            tx_pool_url: "http://localhost:9000/".into(),
            tx_pool_cache_duration: 5,
            tx_pool_poll_interval: 5,
            oauth_client_id: "some_client_id".into(),
            oauth_client_secret: "some_client_secret".into(),
            oauth_authenticate_url: "http://localhost:8080".into(),
            oauth_token_url: "http://localhost:8080".into(),
            oauth_audience: "https://transactions.holesky.signet.sh".into(),
            tx_broadcast_urls: vec!["http://localhost:9000".into()],
        };
        Ok((BlockBuilder::new(&config), config))
    }
}
