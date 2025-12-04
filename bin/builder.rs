use builder::{
    service::serve_builder,
    tasks::{
        block::sim::SimulatorTask, cache::CacheTasks, env::EnvTask, metrics::MetricsTask,
        submit::FlashbotsTask,
    },
};
use init4_bin_base::deps::tracing::{info, info_span};
use tokio::select;

// Note: Must be set to `multi_thread` to support async tasks.
// See: https://docs.rs/tokio/latest/tokio/attr.main.html
#[tokio::main(flavor = "multi_thread")]
async fn main() -> eyre::Result<()> {
    let _guard = init4_bin_base::init4();
    let init_span_guard = info_span!("builder initialization");
    let config = builder::config_from_env();

    let (host_provider, ru_provider, quincey, flashbots, builder_key) = tokio::try_join!(
        config.connect_host_provider(),
        config.connect_ru_provider(),
        config.connect_quincey(),
        config.connect_flashbots(),
        config.connect_builder_signer(),
    )?;

    // Set up env and metrics tasks
    let (env_task, metrics_task) = tokio::try_join!(
        EnvTask::new(host_provider.clone(), ru_provider.clone(), quincey.clone()),
        MetricsTask::new()
    )?;

    // Spawn the env and metrics tasks
    let (block_env, env_jh) = env_task.spawn();
    let (tx_channel, metrics_jh) = metrics_task.spawn();

    // Set up the cache, submit, and simulator tasks
    let cache_tasks = CacheTasks::new(block_env.clone());
    let submit_task = FlashbotsTask::new(
        quincey,
        host_provider.clone(),
        flashbots,
        builder_key,
        tx_channel.clone(),
    )
    .await?;
    let simulator_task = SimulatorTask::new(block_env, host_provider.clone(), ru_provider.clone());

    // Spawn the cache, submit, and simulator tasks
    let cache_system = cache_tasks.spawn();
    let (submit_channel, submit_jh) = submit_task.spawn();
    let build_jh = simulator_task.spawn_simulator_task(cache_system.sim_cache, submit_channel);

    // Start the healthcheck server
    let server = serve_builder(([0, 0, 0, 0], builder::config().builder_port));

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
