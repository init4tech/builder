use builder::{
    tasks::env::EnvTask,
    test_utils::{setup_logging, setup_test_config},
};

#[ignore = "integration test. This test will take between 0 and 12 seconds to run."]
#[tokio::test]
async fn test_bundle_poller_roundtrip() -> eyre::Result<()> {
    setup_logging();
    let _ = setup_test_config();

    let (host, rollup, quincey) = tokio::try_join!(
        builder::config().connect_host_provider(),
        builder::config().connect_ru_provider(),
        builder::config().connect_quincey(),
    )?;
    let (mut env_watcher, _jh) = EnvTask::new(host, rollup, quincey).await?.spawn();

    env_watcher.changed().await.unwrap();
    let env = env_watcher.borrow_and_update();
    assert!(env.as_ref().is_some(), "Env should be Some");

    Ok(())
}
