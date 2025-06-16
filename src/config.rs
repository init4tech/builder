use crate::{
    quincey::Quincey,
    tasks::{
        block::cfg::SignetCfgEnv,
        cache::{BundlePoller, CacheSystem, CacheTask, TxPoller},
        env::{EnvTask, SimEnv},
    },
};
use alloy::{
    network::{Ethereum, EthereumWallet},
    primitives::Address,
    providers::{
        Identity, ProviderBuilder, RootProvider,
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
        signer::{LocalOrAws, SignerError},
    },
};
use signet_zenith::Zenith;
use std::borrow::Cow;
use tokio::sync::watch;

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

/// Configuration for a builder running a specific rollup on a specific host
/// chain.
#[derive(Debug, Clone, FromEnv)]
pub struct BuilderConfig {
    /// The chain ID of the host chain.
    #[from_env(var = "HOST_CHAIN_ID", desc = "The chain ID of the host chain")]
    pub host_chain_id: u64,

    /// The chain ID of the rollup chain.
    #[from_env(var = "RU_CHAIN_ID", desc = "The chain ID of the rollup chain")]
    pub ru_chain_id: u64,

    /// URL for Host RPC node.
    #[from_env(var = "HOST_RPC_URL", desc = "URL for Host RPC node", infallible)]
    pub host_rpc_url: Cow<'static, str>,

    /// URL for the Rollup RPC node.
    #[from_env(var = "ROLLUP_RPC_URL", desc = "URL for Rollup RPC node", infallible)]
    pub ru_rpc_url: Cow<'static, str>,

    /// URL of the tx pool to poll for incoming transactions.
    #[from_env(
        var = "TX_POOL_URL",
        desc = "URL of the tx pool to poll for incoming transactions",
        infallible
    )]
    pub tx_pool_url: Cow<'static, str>,

    /// Additional RPC URLs to which the builder should broadcast transactions.
    /// * Should not include the `HOST_RPC_URL` value, as that is already sent to by default.
    /// * Setting this can incur `already known` errors.
    #[from_env(
        var = "TX_BROADCAST_URLS",
        desc = "Additional RPC URLs to which the builder broadcasts transactions",
        infallible,
        optional
    )]
    pub tx_broadcast_urls: Vec<Cow<'static, str>>,

    /// Address of the Zenith contract on Host.
    #[from_env(var = "ZENITH_ADDRESS", desc = "address of the Zenith contract on Host")]
    pub zenith_address: Address,

    /// Address of the Builder Helper contract on Host.
    #[from_env(
        var = "BUILDER_HELPER_ADDRESS",
        desc = "address of the Builder Helper contract on Host"
    )]
    pub builder_helper_address: Address,

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
    pub concurrency_limit: usize,

    /// The slot calculator for the builder.
    pub slot_calculator: SlotCalculator,
}

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
        static ONCE: std::sync::OnceLock<RootProvider<Ethereum>> = std::sync::OnceLock::new();

        ONCE.get_or_init(|| {
            let url = url::Url::parse(&self.ru_rpc_url).expect("failed to parse URL");
            RootProvider::new_http(url)
        })
        .clone()
    }

    /// Connect to the Host rpc provider.
    pub async fn connect_host_provider(&self) -> eyre::Result<HostProvider> {
        let builder_signer = self.connect_builder_signer().await?;
        ProviderBuilder::new_with_network()
            .disable_recommended_fillers()
            .filler(BlobGasFiller)
            .with_gas_estimation()
            .with_nonce_management(SimpleNonceManager::default())
            .fetch_chain_id()
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

    /// Create an [`EnvTask`] using this config.
    pub fn env_task(&self) -> EnvTask {
        let ru_provider = self.connect_ru_provider();
        EnvTask::new(self.clone(), ru_provider)
    }

    /// Spawn a new [`CacheSystem`] using this config. This contains the
    /// joinhandles for [`TxPoller`] and [`BundlePoller`] and [`CacheTask`], as
    /// well as the [`SimCache`] and the block env watcher.
    ///
    /// [`SimCache`]: signet_sim::SimCache
    pub fn spawn_cache_system(&self, block_env: watch::Receiver<Option<SimEnv>>) -> CacheSystem {
        // Tx Poller pulls transactions from the cache
        let tx_poller = TxPoller::new(self);
        let (tx_receiver, tx_poller) = tx_poller.spawn();

        // Bundle Poller pulls bundles from the cache
        let bundle_poller = BundlePoller::new(self, self.oauth_token());
        let (bundle_receiver, bundle_poller) = bundle_poller.spawn();

        // Set up the cache task
        let cache_task = CacheTask::new(block_env.clone(), bundle_receiver, tx_receiver);
        let (sim_cache, cache_task) = cache_task.spawn();

        CacheSystem { cache_task, tx_poller, bundle_poller, sim_cache }
    }

    /// Create a [`SignetCfgEnv`] using this config.
    pub const fn cfg_env(&self) -> SignetCfgEnv {
        SignetCfgEnv { chain_id: self.ru_chain_id }
    }
}
