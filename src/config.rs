use crate::quincey::Quincey;
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
    perms::{Authenticator, OAuthConfig, SharedToken, pylon},
    utils::{
        calc::SlotCalculator,
        from_env::{EnvItemInfo, FromEnv, OptionalU8WithDefault, OptionalU64WithDefault},
        metrics::MetricsConfig,
        provider::{ProviderConfig, PubSubConfig},
        signer::LocalOrAws,
        tracing::TracingConfig,
    },
};
use itertools::Itertools;
use signet_constants::SignetSystemConstants;
use signet_zenith::Zenith;
use std::borrow::Cow;
use tokio::join;

/// Pylon client type for blob sidecar submission.
pub type PylonClient = pylon::PylonClient;

/// Type alias for the provider used to simulate against rollup state.
pub type RuProvider = RootProvider<Ethereum>;

/// A [`Zenith`] contract instance using [`HostProvider`] as the default provider.
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
    RootProvider,
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
        desc = "The max number of simultaneous block simulations to run [default: std::thread::available_parallelism]",
        optional
    )]
    pub concurrency_limit: Option<usize>,

    /// Optional maximum host gas coefficient to use when building blocks.
    /// Defaults to 80% (80) if not set.
    #[from_env(
        var = "MAX_HOST_GAS_COEFFICIENT",
        desc = "Optional maximum host gas coefficient, as a percentage, to use when building blocks [default: 80]",
        optional
    )]
    pub max_host_gas_coefficient: OptionalU8WithDefault<80>,

    /// Number of milliseconds before the end of the slot to stop querying for new blocks and start the block signing and submission process.
    #[from_env(
        var = "BLOCK_QUERY_CUTOFF_BUFFER",
        desc = "Number of milliseconds before the end of the slot to stop querying for new transactions and start the block signing and submission process. Quincey will stop accepting signature requests 2000ms before the end of the slot, so this buffer should be no less than 2000ms to match. [default: 3000]",
        optional
    )]
    pub block_query_cutoff_buffer: OptionalU64WithDefault<3000>,

    /// Number of milliseconds before the end of the slot by which bundle submission to Flashbots must complete.
    /// If submission completes after this deadline, a warning is logged.
    #[from_env(
        var = "SUBMIT_DEADLINE_BUFFER",
        desc = "Number of milliseconds before the end of the slot by which bundle submission must complete. Submissions that miss this deadline will be logged as warnings. [default: 500]",
        optional
    )]
    pub submit_deadline_buffer: OptionalU64WithDefault<500>,

    /// The slot calculator for the builder.
    pub slot_calculator: SlotCalculator,

    /// The signet system constants.
    pub constants: SignetSystemConstants,

    /// URL for the Pylon blob server API.
    #[from_env(var = "PYLON_URL", desc = "URL for the Pylon blob server API")]
    pub pylon_url: url::Url,

    /// Tracing and OTEL configuration.
    pub tracing: TracingConfig,

    /// Metrics configuration.
    pub metrics: MetricsConfig,
}

impl BuilderConfig {
    /// Sanitizes URL fields to ensure consistent behavior with `url.join()`.
    ///
    /// This ensures `tx_pool_url` has a trailing slash so that `url.join("path")`
    /// appends to the URL path rather than replacing the last segment.
    /// For example:
    /// - `http://example.com/api/` + `join("transactions")` = `http://example.com/api/transactions`
    /// - `http://example.com/api` + `join("transactions")` = `http://example.com/transactions` (wrong!)
    pub fn sanitize(mut self) -> Self {
        // Ensure tx_pool_url has a trailing slash for correct url.join() behavior
        if !self.tx_pool_url.path().ends_with('/') {
            self.tx_pool_url.set_path(&format!("{}/", self.tx_pool_url.path()));
        }
        self
    }

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
    /// is not already running.
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
    pub const fn max_host_gas(&self, gas_limit: u64) -> u64 {
        // Set max host gas to a percentage of the host block gas limit
        ((gas_limit as u128 * self.max_host_gas_coefficient.into_inner() as u128) / 100u128) as u64
    }

    /// Connect to the Pylon blob server.
    pub fn connect_pylon(&self) -> PylonClient {
        PylonClient::new(self.pylon_url.clone(), self.oauth_token())
    }
}

/// Get a list of the env vars used to configure the app.
pub fn env_var_info() -> String {
    // We need to remove the `SlotCalculator` env vars from the list. `SignetSystemConstants`
    // already requires `CHAIN_NAME`, so we don't want to include `CHAIN_NAME` twice.  That also
    // means the other `SlotCalculator` env vars are ignored since `CHAIN_NAME` must be set.
    let is_not_from_slot_calc = |env_item: &&EnvItemInfo| match env_item.var {
        "CHAIN_NAME" if env_item.optional => false,
        "START_TIMESTAMP" | "SLOT_OFFSET" | "SLOT_DURATION" => false,
        _ => true,
    };
    let inventory_iter = BuilderConfig::inventory().into_iter().filter(is_not_from_slot_calc);
    let max_width = inventory_iter.clone().map(|env_item| env_item.var.len()).max().unwrap_or(0);
    inventory_iter
        .map(|env_item| {
            format!(
                "  {:width$}  {}{}",
                env_item.var,
                env_item.description,
                if env_item.optional { " [optional]" } else { "" },
                width = max_width
            )
        })
        .join("\n")
}

#[cfg(test)]
mod tests {
    /// Tests that URL sanitization correctly handles trailing slashes for url.join() behavior.
    ///
    /// The `url.join()` method behaves differently based on trailing slashes:
    /// - With trailing slash: `http://example.com/api/`.join("path") = `http://example.com/api/path`
    /// - Without trailing slash: `http://example.com/api`.join("path") = `http://example.com/path`
    ///
    /// This test verifies that our sanitization ensures consistent behavior.
    mod tx_pool_url_sanitization {
        use url::Url;

        #[test]
        fn root_url_already_has_trailing_slash() {
            // Per URL spec, a URL without an explicit path gets the root path "/"
            // So "http://localhost:9000" is equivalent to "http://localhost:9000/"
            let url: Url = "http://localhost:9000".parse().unwrap();
            assert_eq!(url.path(), "/");
            assert!(url.path().ends_with('/'));

            // Sanitization is a no-op for root URLs since they already have trailing slash
            let mut sanitized = url.clone();
            if !sanitized.path().ends_with('/') {
                sanitized.set_path(&format!("{}/", sanitized.path()));
            }

            assert!(sanitized.path().ends_with('/'));
            assert_eq!(sanitized.as_str(), "http://localhost:9000/");
        }

        #[test]
        fn url_with_trailing_slash_unchanged() {
            let url: Url = "http://localhost:9000/".parse().unwrap();
            assert!(url.path().ends_with('/'));

            // Simulate sanitization - should be unchanged
            let mut sanitized = url.clone();
            if !sanitized.path().ends_with('/') {
                sanitized.set_path(&format!("{}/", sanitized.path()));
            }

            assert_eq!(sanitized.as_str(), "http://localhost:9000/");
        }

        #[test]
        fn url_with_path_without_trailing_slash_gets_sanitized() {
            let url: Url = "http://localhost:9000/api/v1".parse().unwrap();
            assert!(!url.path().ends_with('/'));

            // Simulate sanitization
            let mut sanitized = url.clone();
            if !sanitized.path().ends_with('/') {
                sanitized.set_path(&format!("{}/", sanitized.path()));
            }

            assert!(sanitized.path().ends_with('/'));
            assert_eq!(sanitized.as_str(), "http://localhost:9000/api/v1/");
        }

        #[test]
        fn url_with_path_with_trailing_slash_unchanged() {
            let url: Url = "http://localhost:9000/api/v1/".parse().unwrap();
            assert!(url.path().ends_with('/'));

            // Simulate sanitization - should be unchanged
            let mut sanitized = url.clone();
            if !sanitized.path().ends_with('/') {
                sanitized.set_path(&format!("{}/", sanitized.path()));
            }

            assert_eq!(sanitized.as_str(), "http://localhost:9000/api/v1/");
        }

        #[test]
        fn sanitized_url_joins_correctly() {
            // Without sanitization - WRONG behavior
            let url_no_slash: Url = "http://localhost:9000/api".parse().unwrap();
            let joined_wrong = url_no_slash.join("transactions").unwrap();
            assert_eq!(joined_wrong.as_str(), "http://localhost:9000/transactions");

            // With sanitization - CORRECT behavior
            let url_with_slash: Url = "http://localhost:9000/api/".parse().unwrap();
            let joined_correct = url_with_slash.join("transactions").unwrap();
            assert_eq!(joined_correct.as_str(), "http://localhost:9000/api/transactions");
        }

        #[test]
        fn sanitized_root_url_joins_correctly() {
            // Root URL without trailing slash
            let url_no_slash: Url = "http://localhost:9000".parse().unwrap();

            // Sanitize it
            let mut sanitized = url_no_slash.clone();
            if !sanitized.path().ends_with('/') {
                sanitized.set_path(&format!("{}/", sanitized.path()));
            }

            // Now join should work correctly
            let joined = sanitized.join("transactions").unwrap();
            assert_eq!(joined.as_str(), "http://localhost:9000/transactions");
        }
    }
}
