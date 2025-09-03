use builder::{
    tasks::env::EnvTask,
    test_utils::{setup_logging, setup_test_config},
};
use signet_constants::SignetSystemConstants;

#[ignore = "integration test. This test will take between 0 and 12 seconds to run."]
#[tokio::test]
async fn test_bundle_poller_roundtrip() -> eyre::Result<()> {
    setup_logging();

    let config = setup_test_config().unwrap();
    let (mut env_watcher, _jh) = EnvTask::new(
        config.clone(),
        SignetSystemConstants::pecorino(),
        config.connect_host_provider().await?,
        config.connect_ru_provider().await?,
    )
    .spawn();

    env_watcher.changed().await.unwrap();
    let env = env_watcher.borrow_and_update();
    assert!(env.as_ref().is_some(), "Env should be Some");

    Ok(())
}
