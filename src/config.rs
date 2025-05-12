use crate::{
    quincey::Quincey,
    signer::{LocalOrAws, SignerError},
    tasks::oauth::{Authenticator, SharedToken},
};
use alloy::{
    network::{Ethereum, EthereumWallet},
    primitives::Address,
    providers::{
        Identity, ProviderBuilder, RootProvider,
        fillers::{
            BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller,
            WalletFiller,
        },
    },
};
use eyre::Result;
use init4_bin_base::utils::{calc::SlotCalculator, from_env::FromEnv};
use oauth2::url;
use signet_zenith::Zenith;
use std::borrow::Cow;

/// Type alias for the provider used to build and submit blocks to the host.
pub type HostProvider = FillProvider<
    JoinFill<
        JoinFill<
            Identity,
            JoinFill<GasFiller, JoinFill<BlobGasFiller, JoinFill<NonceFiller, ChainIdFiller>>>,
        >,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider,
    Ethereum,
>;

/// Configuration for a builder running a specific rollup on a specific host
/// chain.
#[derive(serde::Deserialize, Debug, Clone, FromEnv)]
pub struct BuilderConfig {
    /// The chain ID of the host chain
    #[from_env(var = "HOST_CHAIN_ID", desc = "The chain ID of the host chain")]
    pub host_chain_id: u64,
    /// The chain ID of the rollup chain
    #[from_env(var = "RU_CHAIN_ID", desc = "The chain ID of the rollup chain")]
    pub ru_chain_id: u64,
    /// URL for Host RPC node.
    #[from_env(var = "HOST_RPC_URL", desc = "URL for Host RPC node", infallible)]
    pub host_rpc_url: Cow<'static, str>,
    /// URL for the Rollup RPC node.
    #[from_env(var = "ROLLUP_RPC_URL", desc = "URL for Rollup RPC node", infallible)]
    pub ru_rpc_url: Cow<'static, str>,
    /// Additional RPC URLs to which to broadcast transactions.
    /// NOTE: should not include the host_rpc_url value
    #[from_env(
        var = "TX_BROADCAST_URLS",
        desc = "Additional RPC URLs to which to broadcast transactions",
        infallible
    )]
    pub tx_broadcast_urls: Vec<Cow<'static, str>>,
    /// address of the Zenith contract on Host.
    #[from_env(var = "ZENITH_ADDRESS", desc = "address of the Zenith contract on Host")]
    pub zenith_address: Address,
    /// address of the Builder Helper contract on Host.
    #[from_env(
        var = "BUILDER_HELPER_ADDRESS",
        desc = "address of the Builder Helper contract on Host"
    )]
    pub builder_helper_address: Address,
    /// URL for remote Quincey Sequencer server to sign blocks.
    /// Disregarded if a sequencer_signer is configured.
    #[from_env(
        var = "QUINCEY_URL",
        desc = "URL for remote Quincey Sequencer server to sign blocks",
        infallible
    )]
    pub quincey_url: Cow<'static, str>,
    /// Port for the Builder server.
    #[from_env(var = "BUILDER_PORT", desc = "Port for the Builder server")]
    pub builder_port: u16,
    /// Key to access Sequencer Wallet - AWS Key ID _OR_ local private key.
    /// Set IFF using local Sequencer signing instead of remote Quincey signing.
    #[from_env(
        var = "SEQUENCER_KEY",
        desc = "Key to access Sequencer Wallet - AWS Key ID _OR_ local private key, set IFF using local Sequencer signing instead of remote Quincey signing",
        infallible,
        optional
    )]
    pub sequencer_key: Option<String>,
    /// Key to access Builder transaction submission wallet - AWS Key ID _OR_ local private key.
    #[from_env(
        var = "BUILDER_KEY",
        desc = "Key to access Builder transaction submission wallet - AWS Key ID _OR_ local private key",
        infallible
    )]
    pub builder_key: String,
    /// Buffer in seconds in which the `submitBlock` transaction must confirm on the Host chain.
    #[from_env(
        var = "BLOCK_CONFIRMATION_BUFFER",
        desc = "Buffer in seconds in which the `submitBlock` transaction must confirm on the Host chain"
    )]
    pub block_confirmation_buffer: u64,

    /// Address on Rollup to which Builder will receive user transaction fees.
    #[from_env(
        var = "BUILDER_REWARDS_ADDRESS",
        desc = "Address on Rollup to which Builder will receive user transaction fees"
    )]
    pub builder_rewards_address: Address,
    /// Gas limit for RU block.
    /// NOTE: a "smart" builder would determine this programmatically by simulating the block.
    #[from_env(var = "ROLLUP_BLOCK_GAS_LIMIT", desc = "Gas limit for RU block")]
    pub rollup_block_gas_limit: u64,
    /// URL of the tx pool to poll for incoming transactions.
    #[from_env(
        var = "TX_POOL_URL",
        desc = "URL of the tx pool to poll for incoming transactions",
        infallible
    )]
    pub tx_pool_url: Cow<'static, str>,
    /// Duration in seconds transactions can live in the tx-pool cache.
    #[from_env(
        var = "TX_POOL_CACHE_DURATION",
        desc = "Duration in seconds transactions can live in the tx-pool cache"
    )]
    pub tx_pool_cache_duration: u64,
    /// OAuth client ID for the builder.
    #[from_env(var = "OAUTH_CLIENT_ID", desc = "OAuth client ID for the builder")]
    pub oauth_client_id: String,
    /// OAuth client secret for the builder.
    #[from_env(var = "OAUTH_CLIENT_SECRET", desc = "OAuth client secret for the builder")]
    pub oauth_client_secret: String,
    /// OAuth authenticate URL for the builder for performing OAuth logins.
    #[from_env(
        var = "OAUTH_AUTHENTICATE_URL",
        desc = "OAuth authenticate URL for the builder for performing OAuth logins"
    )]
    pub oauth_authenticate_url: String,
    /// OAuth token URL for the builder to get an OAuth2 access token
    #[from_env(
        var = "OAUTH_TOKEN_URL",
        desc = "OAuth token URL for the builder to get an OAuth2 access token"
    )]
    pub oauth_token_url: String,
    /// The oauth token refresh interval in seconds.
    #[from_env(
        var = "AUTH_TOKEN_REFRESH_INTERVAL",
        desc = "The oauth token refresh interval in seconds"
    )]
    pub oauth_token_refresh_interval: u64,
    /// The max number of simultaneous block simulations to run.
    #[from_env(
        var = "CONCURRENCY_LIMIT",
        desc = "The max number of simultaneous block simulations to run"
    )]
    pub concurrency_limit: usize,

    /// The slot calculator for the builder.
    pub slot_calculator: SlotCalculator,
}

/// Type alias for the provider used to simulate against rollup state.
pub type RuProvider = RootProvider<Ethereum>;

/// A [`Zenith`] contract instance using [`Provider`] as the provider.
pub type ZenithInstance<P = HostProvider> = Zenith::ZenithInstance<(), P, alloy::network::Ethereum>;

impl BuilderConfig {
    /// Connect to the Builder signer.
    pub async fn connect_builder_signer(&self) -> Result<LocalOrAws, SignerError> {
        LocalOrAws::load(&self.builder_key, Some(self.host_chain_id)).await
    }

    /// Connect to the Sequencer signer.
    pub async fn connect_sequencer_signer(&self) -> eyre::Result<Option<LocalOrAws>> {
        if let Some(sequencer_key) = &self.sequencer_key {
            LocalOrAws::load(sequencer_key, Some(self.host_chain_id))
                .await
                .map_err(Into::into)
                .map(Some)
        } else {
            Ok(None)
        }
    }

    /// Connect to the Rollup rpc provider.
    pub fn connect_ru_provider(&self) -> RootProvider<Ethereum> {
        let url = url::Url::parse(&self.ru_rpc_url).expect("failed to parse URL");
        RootProvider::<Ethereum>::new_http(url)
    }

    /// Connect to the Host rpc provider.
    pub async fn connect_host_provider(&self) -> eyre::Result<HostProvider> {
        let builder_signer = self.connect_builder_signer().await?;
        ProviderBuilder::new()
            .wallet(EthereumWallet::from(builder_signer))
            .connect(&self.host_rpc_url)
            .await
            .map_err(Into::into)
    }

    /// Connect additional broadcast providers.
    pub fn connect_additional_broadcast(&self) -> Vec<RootProvider> {
        self.tx_broadcast_urls
            .iter()
            .map(|url_str| {
                let url = url::Url::parse(url_str).expect("failed to parse URL");
                RootProvider::new_http(url)
            })
            .collect::<Vec<_>>()
    }

    /// Connect to the Zenith instance, using the specified provider.
    pub const fn connect_zenith(&self, provider: HostProvider) -> ZenithInstance {
        Zenith::new(self.zenith_address, provider)
    }

    /// Get an oauth2 token for the builder, starting the authenticator if it
    // is not already running.
    pub fn oauth_token(&self) -> SharedToken {
        static ONCE: std::sync::OnceLock<SharedToken> = std::sync::OnceLock::new();

        ONCE.get_or_init(|| {
            let authenticator = Authenticator::new(self).unwrap();
            let token = authenticator.token();
            authenticator.spawn();
            token
        })
        .clone()
    }

    /// Connect to a Quincey, owned or shared.
    pub async fn connect_quincey(&self) -> eyre::Result<Quincey> {
        if let Some(signer) = self.connect_sequencer_signer().await? {
            return Ok(Quincey::new_owned(signer));
        }

        let client = reqwest::Client::new();
        let url = url::Url::parse(&self.quincey_url)?;
        let token = self.oauth_token();

        Ok(Quincey::new_remote(client, url, token))
    }
}
