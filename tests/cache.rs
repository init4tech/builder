use builder::{
    tasks::{cache::CacheTasks, env::EnvTask},
    test_utils::{setup_logging, setup_test_config},
};
use init4_bin_base::deps::tracing::warn;
use std::time::Duration;

#[ignore = "integration test. This test will take >12 seconds to run, and requires Authz configuration env vars."]
#[tokio::test]
async fn test_bundle_poller_roundtrip() -> eyre::Result<()> {
    setup_logging();

    let config = setup_test_config().unwrap();

    let (block_env, _jh) = EnvTask::new(
        config.clone(),
        config.connect_host_provider().await?,
        config.connect_ru_provider().await?,
    )
    .spawn();
    let cache_tasks = CacheTasks::new(config.clone(), block_env);
    let cache_system = cache_tasks.spawn();

    tokio::time::sleep(Duration::from_secs(12)).await;

    warn!(txns = ?cache_system.sim_cache.read_best(5));

    Ok(())
}
