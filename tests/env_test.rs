use std::time::Duration;

use builder::{
    tasks::env::EnvTask,
    test_utils::{setup_logging, setup_test_config},
};
use tokio::time::sleep;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_handle_build() {
    setup_logging();

    // Make a test config pointed at the host parmigiana rpc endpoint
    let config = setup_test_config();
    config.host_rpc.connect().await.expect("host rpc");
    config.ru_rpc.connect().await.expect("ru rpc");

    let env_task = EnvTask::new().await.expect("env task");
    let (mut env_receiver, jh) = env_task.spawn();

    while env_receiver.borrow().is_none() {
        env_receiver.changed().await.expect("env task dropped");
    }

    loop {
        let sim_env = env_receiver.borrow().as_ref().unwrap().clone();
        println!(
            "Test received SimEnv: rollup block number {}, host block number {}",
            sim_env.rollup_env().number,
            sim_env.host_env().number
        );
        sleep(Duration::from_secs(1)).await;
    }
}
