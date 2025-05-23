use builder::{
    config::BuilderConfig,
    service::serve_builder,
    tasks::{
        block::sim::Simulator,
        cache::{BundlePoller, TxPoller},
        metrics::MetricsTask,
        submit::SubmitTask,
    },
};
use init4_bin_base::{deps::tracing, utils::from_env::FromEnv};
use signet_sim::SimCache;
use signet_types::constants::SignetSystemConstants;
use tokio::select;
use tracing::info_span;

// Note: Must be set to `multi_thread` to support async tasks.
// See: https://docs.rs/tokio/latest/tokio/attr.main.html
#[tokio::main(flavor = "multi_thread")]
async fn main() -> eyre::Result<()> {
    let _guard = init4_bin_base::init4();
    let init_span_guard = info_span!("builder initialization");

    let config = BuilderConfig::from_env()?.clone();
    let constants = SignetSystemConstants::pecorino();
    let token = config.oauth_token();

    let (host_provider, quincey) =
        tokio::try_join!(config.connect_host_provider(), config.connect_quincey())?;
    let ru_provider = config.connect_ru_provider();

    let zenith = config.connect_zenith(host_provider.clone());

    let metrics = MetricsTask { host_provider };
    let (tx_channel, metrics_jh) = metrics.spawn();

    let submit =
        SubmitTask { zenith, quincey, config: config.clone(), outbound_tx_channel: tx_channel };

    let tx_poller = TxPoller::new(&config);
    let (tx_receiver, tx_poller_jh) = tx_poller.spawn();

    let bundle_poller = BundlePoller::new(&config, token);
    let (bundle_receiver, bundle_poller_jh) = bundle_poller.spawn();

    let (submit_channel, submit_jh) = submit.spawn();

    let sim_items = SimCache::new();
    let slot_calculator = config.slot_calculator;

    let sim = Simulator::new(&config, ru_provider.clone(), slot_calculator);

    let (basefee_jh, sim_cache_jh) =
        sim.spawn_cache_tasks(tx_receiver, bundle_receiver, sim_items.clone());

    let build_jh = sim.spawn_simulator_task(constants, sim_items.clone(), submit_channel);

    let server = serve_builder(([0, 0, 0, 0], config.builder_port));

    // We have finished initializing the builder, so we can drop the init span
    // guard.
    drop(init_span_guard);

    select! {
        _ = tx_poller_jh => {
            tracing::info!("tx_poller finished");
        },
        _ = bundle_poller_jh => {
            tracing::info!("bundle_poller finished");
        },
        _ = sim_cache_jh => {
            tracing::info!("sim cache task finished");
        }
        _ = basefee_jh => {
            tracing::info!("basefee task finished");
        }
        _ = submit_jh => {
            tracing::info!("submit finished");
        },
        _ = metrics_jh => {
            tracing::info!("metrics finished");
        },
        _ = build_jh => {
            tracing::info!("build finished");
        }
        _ = server => {
            tracing::info!("server finished");
        }
    }

    tracing::info!("shutting down");

    Ok(())
}
