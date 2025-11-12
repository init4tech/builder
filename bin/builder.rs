use builder::{
    config::BuilderConfig,
    service::serve_builder,
    tasks::{
        block::sim::Simulator, cache::CacheTasks, env::EnvTask, metrics::MetricsTask,
        submit::FlashbotsTask,
    },
};
use init4_bin_base::{
    deps::tracing::{info, info_span},
    utils::from_env::FromEnv,
};
use tokio::select;

// Note: Must be set to `multi_thread` to support async tasks.
// See: https://docs.rs/tokio/latest/tokio/attr.main.html
#[tokio::main(flavor = "multi_thread")]
async fn main() -> eyre::Result<()> {
    let _guard = init4_bin_base::init4();
    let init_span_guard = info_span!("builder initialization");

    // Pull the configuration from the environment
    let config = BuilderConfig::from_env()?.clone();

    // We connect the providers greedily, so we can fail early if the
    // RU WS connection is invalid.
    let (ru_provider, host_provider) =
        tokio::try_join!(config.connect_ru_provider(), config.connect_host_provider(),)?;
    let quincey = config.connect_quincey().await?;

    // Spawn the EnvTask
    let env_task =
        EnvTask::new(config.clone(), host_provider.clone(), quincey, ru_provider.clone());
    let (block_env, env_jh) = env_task.spawn();

    // Spawn the cache system
    let cache_tasks = CacheTasks::new(config.clone(), block_env.clone());
    let cache_system = cache_tasks.spawn();

    // Set up the metrics task
    let metrics = MetricsTask::new(host_provider.clone());
    let (tx_channel, metrics_jh) = metrics.spawn();

    // Spawn the Flashbots task
    let submit = FlashbotsTask::new(config.clone(), tx_channel).await?;
    let (submit_channel, submit_jh) = submit.spawn();

    // Set up the simulator
    let sim = Simulator::new(&config, host_provider, ru_provider, block_env);
    let build_jh = sim.spawn_simulator_task(cache_system.sim_cache, submit_channel);

    // Start the healthcheck server
    let server = serve_builder(([0, 0, 0, 0], config.builder_port));

    // We have finished initializing the builder, so we can drop the init span
    // guard.
    drop(init_span_guard);

    select! {

        _ = env_jh => {
            info!("env task finished");
        },
        _ = cache_system.cache_task => {
            info!("cache task finished");
        },
        _ = cache_system.tx_poller => {
            info!("tx_poller finished");
        },
        _ = cache_system.bundle_poller => {
            info!("bundle_poller finished");
        },
        _ = submit_jh => {
            info!("submit finished");
        },
        _ = metrics_jh => {
            info!("metrics finished");
        },
        _ = build_jh => {
            info!("build finished");
        }
        _ = server => {
            info!("server finished");
        }
    }

    info!("shutting down");

    Ok(())
}
