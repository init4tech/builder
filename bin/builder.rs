#![allow(dead_code)]

use builder::config::BuilderConfig;
use builder::service::serve_builder_with_span;
use builder::tasks::bundler::BundlePoller;
use builder::tasks::oauth::Authenticator;
use builder::tasks::tx_poller::TxPoller;

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

    let port = config.builder_port;
    let tx_poller = TxPoller::new(&config);
    let bundle_poller = BundlePoller::new(&config, authenticator.clone()).await;
    let builder = builder::tasks::block::BlockBuilder::new(&config);

    let submit = builder::tasks::submit::SubmitTask {
        authenticator: authenticator.clone(),
        provider,
        zenith,
        client: reqwest::Client::new(),
        sequencer_signer,
        config: config.clone(),
    };

    let authenticator_jh = authenticator.spawn();
    let (submit_channel, submit_jh) = submit.spawn();
    let (tx_channel, bundle_channel, build_jh) = builder.spawn(submit_channel);
    let tx_poller_jh = tx_poller.spawn(tx_channel.clone());
    let bundle_poller_jh = bundle_poller.spawn(bundle_channel);

    let server = serve_builder_with_span(tx_channel, ([0, 0, 0, 0], port), span);

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
        _ = tx_poller_jh => {
            tracing::info!("tx_poller finished");
        }
        _ = bundle_poller_jh => {
            tracing::info!("bundle_poller finished");
        }
        _ = authenticator_jh => {
            tracing::info!("authenticator finished");
        }
    }

    tracing::info!("shutting down");

    Ok(())
}
