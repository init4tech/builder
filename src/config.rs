use crate::quincey::Quincey;
use alloy::{
    network::{Ethereum, EthereumWallet},
    primitives::Address,
    providers::{
        self, Identity, ProviderBuilder, RootProvider,
        fillers::{
            BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller,
            SimpleNonceManager, WalletFiller,
        },
    },
};
use eyre::Result;
use init4_bin_base::{
    perms::{Authenticator, OAuthConfig, SharedToken},
    utils::{
        calc::SlotCalculator,
        from_env::FromEnv,
        provider::{ProviderConfig, PubSubConfig},
        signer::LocalOrAws,
    },
};
use signet_constants::SignetSystemConstants;
use signet_zenith::Zenith;
use std::borrow::Cow;
use tokio::join;

/// Type alias for the provider used to simulate against rollup state.
pub type RuProvider = RootProvider<Ethereum>;

/// A [`Zenith`] contract instance using [`Provider`] as the provider.
pub type ZenithInstance<P = HostProvider> = Zenith::ZenithInstance<P, alloy::network::Ethereum>;

/// Type alias for the provider used to build and submit blocks to the host.
pub type HostProvider = FillProvider<
    JoinFill<
        JoinFill<
            JoinFill<
                JoinFill<JoinFill<Identity, BlobGasFiller>, GasFiller>,
                NonceFiller<SimpleNonceManager>,
            >,
            ChainIdFiller,
        >,
        WalletFiller<EthereumWallet>,
    >,
    RootProvider,
>;

/// The provider type used to submit bundles to a Flashbots relay.
pub type FlashbotsProvider = FillProvider<
    JoinFill<
        JoinFill<
            Identity,
            JoinFill<GasFiller, JoinFill<BlobGasFiller, JoinFill<NonceFiller, ChainIdFiller>>>,
        >,
        WalletFiller<EthereumWallet>,
    >,
    providers::RootProvider,
>;

/// The default concurrency limit for the builder if the system call
/// fails and no user-specified value is set.
pub const DEFAULT_CONCURRENCY_LIMIT: usize = 8;

/// Configuration for a builder running a specific rollup on a specific host
/// chain.
#[derive(Debug, Clone, FromEnv)]
pub struct BuilderConfig {
    /// URL for Host RPC node.
    #[from_env(
        var = "HOST_RPC_URL",
        desc = "URL for Host RPC node. This MUST be a valid HTTP or WS URL, starting with http://, https://, ws:// or wss://"
    )]
    pub host_rpc: ProviderConfig,

    /// URL for the Rollup RPC node.
    #[from_env(
        var = "ROLLUP_RPC_URL",
        desc = "URL for Rollup RPC node. This MUST be a valid WS url starting with ws:// or wss://. Http providers are not supported."
    )]
    pub ru_rpc: PubSubConfig,

    /// URL of the tx pool to poll for incoming transactions.
    #[from_env(var = "TX_POOL_URL", desc = "URL of the tx pool to poll for incoming transactions")]
    pub tx_pool_url: url::Url,

    /// Configuration for the Flashbots provider to submit
    /// SignetBundles and Rollup blocks to the Host chain
    /// as private MEV bundles via Flashbots.
    #[from_env(
        var = "FLASHBOTS_ENDPOINT",
        desc = "Flashbots endpoint for privately submitting Signet bundles"
    )]
    pub flashbots_endpoint: url::Url,

    /// URL for remote Quincey Sequencer server to sign blocks.
    /// NB: Disregarded if a sequencer_signer is configured.
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

    /// Oauth2 configuration for the builder to connect to init4 services.
    pub oauth: OAuthConfig,

    /// The max number of simultaneous block simulations to run.
    #[from_env(
        var = "CONCURRENCY_LIMIT",
        desc = "The max number of simultaneous block simulations to run"
    )]
    pub concurrency_limit: Option<usize>,

    /// Optional maximum host gas coefficient to use when building blocks.
    /// Defaults to 80% (80) if not set.
    #[from_env(
        var = "MAX_HOST_GAS_COEFFICIENT",
        desc = "Optional maximum host gas coefficient, as a percentage, to use when building blocks",
        default = 80
    )]
    pub max_host_gas_coefficient: Option<u8>,

    /// Number of milliseconds before the end of the slot to stop querying for new blocks and start the block signing and submission process.
    #[from_env(
        var = "BLOCK_QUERY_CUTOFF_BUFFER",
        desc = "Number of milliseconds before the end of the slot to stop querying for new transactions and start the block signing and submission process. Quincey will stop accepting signature requests 2000ms before the end of the slot, so this buffer should be no less than 2000ms to match.",
        default = 3000
    )]
    pub block_query_cutoff_buffer: u64,

    /// The slot calculator for the builder.
    pub slot_calculator: SlotCalculator,

    /// The signet system constants.
    pub constants: SignetSystemConstants,
}

impl BuilderConfig {
    /// Connect to the Builder signer.
    pub async fn connect_builder_signer(&self) -> eyre::Result<LocalOrAws> {
        static ONCE: tokio::sync::OnceCell<LocalOrAws> = tokio::sync::OnceCell::const_new();

        ONCE.get_or_try_init(|| async {
            LocalOrAws::load(&self.builder_key, Some(self.constants.host_chain_id())).await
        })
        .await
        .cloned()
        .map_err(Into::into)
    }

    /// Connect to the Sequencer signer.
    pub async fn connect_sequencer_signer(&self) -> eyre::Result<Option<LocalOrAws>> {
        if let Some(sequencer_key) = &self.sequencer_key {
            LocalOrAws::load(sequencer_key, Some(self.constants.host_chain_id()))
                .await
                .map_err(Into::into)
                .map(Some)
        } else {
            Ok(None)
        }
    }

    /// Connect to the Rollup rpc provider.
    pub async fn connect_ru_provider(&self) -> eyre::Result<RootProvider<Ethereum>> {
        static ONCE: tokio::sync::OnceCell<RootProvider<Ethereum>> =
            tokio::sync::OnceCell::const_new();

        ONCE.get_or_try_init(|| async {
            RootProvider::connect_with(self.ru_rpc.clone()).await.map_err(Into::into)
        })
        .await
        .cloned()
    }

    /// Connect to the Host rpc provider.
    pub async fn connect_host_provider(&self) -> eyre::Result<HostProvider> {
        let (provider, builder_signer) =
            join!(self.host_rpc.connect(), self.connect_builder_signer());

        Ok(ProviderBuilder::new_with_network()
            .disable_recommended_fillers()
            .filler(BlobGasFiller::default())
            .with_gas_estimation()
            .with_nonce_management(SimpleNonceManager::default())
            .fetch_chain_id()
            .wallet(EthereumWallet::from(builder_signer?))
            .connect_provider(provider?))
    }

    /// Connect to a Flashbots bundle provider.
    pub async fn connect_flashbots(&self) -> Result<FlashbotsProvider> {
        self.connect_builder_signer().await.map(|signer| {
            ProviderBuilder::new().wallet(signer).connect_http(self.flashbots_endpoint.clone())
        })
    }

    /// Connect to the Zenith instance, using the specified provider.
    pub const fn connect_zenith(&self, provider: HostProvider) -> ZenithInstance {
        Zenith::new(self.constants.host_zenith(), provider)
    }

    /// Get an oauth2 token for the builder, starting the authenticator if it
    // is not already running.
    pub fn oauth_token(&self) -> SharedToken {
        static ONCE: std::sync::OnceLock<SharedToken> = std::sync::OnceLock::new();

        ONCE.get_or_init(|| {
            let authenticator = Authenticator::new(&self.oauth);
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

    /// Memoizes the concurrency limit for the current system. Uses [`std::thread::available_parallelism`] if no
    /// value is set. If that for some reason fails, it returns the default concurrency limit.
    pub fn concurrency_limit(&self) -> usize {
        static ONCE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

        if let Some(limit) = self.concurrency_limit
            && limit > 0
        {
            return limit;
        }

        *ONCE.get_or_init(|| {
            std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(DEFAULT_CONCURRENCY_LIMIT)
        })
    }

    /// Returns the maximum host gas to use for block building based on the configured max host gas coefficient.
    pub fn max_host_gas(&self, gas_limit: u64) -> u64 {
        // Set max host gas to a percentage of the host block gas limit
        ((gas_limit as u128 * (self.max_host_gas_coefficient.unwrap_or(80) as u128)) / 100u128)
            as u64
    }
}
