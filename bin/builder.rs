#![allow(dead_code)]

use builder::config::BuilderConfig;
use builder::service::serve_builder_with_span;
use builder::tasks::block::BlockBuilder;
use builder::tasks::oauth::Authenticator;
use builder::tasks::submit::SubmitTask;

use tokio::select;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::try_init().unwrap();
    let span = tracing::info_span!("zenith-builder");

    let config = BuilderConfig::load_from_env()?.clone();
    let provider = config.connect_provider().await?;
    let authenticator = Authenticator::new(&config);

    tracing::debug!(rpc_url = config.host_rpc_url.as_ref(), "instantiated provider");

    let sequencer_signer = config.connect_sequencer_signer().await?;
    let zenith = config.connect_zenith(provider.clone());

    let builder = BlockBuilder::new(&config, authenticator.clone());
    let submit = SubmitTask {
        authenticator: authenticator.clone(),
        provider,
        zenith,
        client: reqwest::Client::new(),
        sequencer_signer,
        config: config.clone(),
    };

    let authenticator_jh = authenticator.spawn();
    let (submit_channel, submit_jh) = submit.spawn();
    let build_jh = builder.spawn(submit_channel);

    let port = config.builder_port;
    let server = serve_builder_with_span(([0, 0, 0, 0], port), span);

    select! {
        _ = submit_jh => {
            tracing::info!("submit finished");
        },
        _ = build_jh => {
            tracing::info!("build finished");
        }
        _ = server => {
            tracing::info!("server finished");
        }
        _ = authenticator_jh => {
            tracing::info!("authenticator finished");
        }
    }

    tracing::info!("shutting down");

    Ok(())
}
