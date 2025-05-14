use builder::test_utils::{setup_logging, setup_test_config};

#[ignore = "integration test. This test will take between 0 and 12 seconds to run."]
#[tokio::test]
async fn test_bundle_poller_roundtrip() {
    setup_logging();

    let config = setup_test_config().unwrap();
    let env_task = config.env_task();
    let (mut env_watcher, _jh) = env_task.spawn();

    env_watcher.changed().await.unwrap();
    let env = env_watcher.borrow_and_update();
    assert!(env.as_ref().is_some(), "Env should be Some");
}
