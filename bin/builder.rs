#![allow(dead_code)]

use builder::config::BuilderConfig;
use builder::service::serve_builder_with_span;
use builder::tasks::block::BlockBuilder;
use builder::tasks::oauth::Authenticator;
use builder::tasks::receipts::ReceiptTask;
use builder::tasks::submit::SubmitTask;
use metrics_exporter_prometheus::PrometheusBuilder;

use tokio::select;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt::try_init().unwrap();
    let span = tracing::info_span!("zenith-builder");

    let config = BuilderConfig::load_from_env()?.clone();
    let host_provider = config.connect_host_provider().await?;
    let ru_provider = config.connect_ru_provider().await?;
    let authenticator = Authenticator::new(&config);

    PrometheusBuilder::new().install().expect("failed to install prometheus exporter");

    tracing::debug!(rpc_url = config.host_rpc_url.as_ref(), "instantiated provider");

    let sequencer_signer = config.connect_sequencer_signer().await?;
    let zenith = config.connect_zenith(host_provider.clone());

    let builder = BlockBuilder::new(&config, authenticator.clone(), ru_provider);

    let receipts = ReceiptTask { host_provider: host_provider.clone() };
    let (tx_channel, receipts_jh) = receipts.spawn();

    let submit = SubmitTask {
        authenticator: authenticator.clone(),
        host_provider,
        zenith,
        client: reqwest::Client::new(),
        sequencer_signer,
        config: config.clone(),
        outbound_tx_channel: tx_channel,
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
        _ = receipts_jh => {
            tracing::info!("receipts finished");
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
