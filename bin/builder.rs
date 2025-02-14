#![allow(dead_code)]

use builder::config::BuilderConfig;
use builder::service::serve_builder_with_span;
use builder::tasks::bundler::BundlePoller;
use builder::tasks::metrics::MetricsTask;
use builder::tasks::oauth::Authenticator;
use builder::tasks::simulator::Simulator;
use builder::tasks::submit::SubmitTask;
use builder::tasks::tx_poller::TxPoller;
use metrics_exporter_prometheus::PrometheusBuilder;

use tokio::select;
use trevm::{NoopBlock, NoopCfg};

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

    // Metrics
    let metrics = MetricsTask { host_provider: host_provider.clone() };
    let (tx_channel, metrics_jh) = metrics.spawn();

    // Submit
    let submit = SubmitTask {
        authenticator: authenticator.clone(),
        host_provider,
        zenith,
        client: reqwest::Client::new(),
        sequencer_signer,
        config: config.clone(),
        outbound_tx_channel: tx_channel,
    };
    let (submit_channel, submit_jh) = submit.spawn();

    // Bundle poller
    let bundle_poller = BundlePoller::new(&config, authenticator.clone());
    let (inbound_bundles, bundle_jh) = bundle_poller.spawn();

    // Transaction poller
    let tx_poller = TxPoller::new(&config);
    let (inbound_txs, tx_jh) = tx_poller.spawn();

    // Authenticator
    let authenticator_jh = authenticator.spawn();

    // Simulator
    let simulator: Simulator<(), NoopCfg, NoopBlock> = Simulator::new(ru_provider, config.clone()).await?;

    let simulator_jh = simulator.spawn(inbound_bundles, inbound_txs, submit_channel);

    // Server
    let port = config.builder_port;
    let server = serve_builder_with_span(([0, 0, 0, 0], port), span);

    // Graceful shutdown
    select! {
        _ = metrics_jh => {
            tracing::info!("metrics finished");
        },
        _ = authenticator_jh => {
            tracing::info!("authenticator finished");
        }
        _ = tx_jh => {
            tracing::info!("tx poller finished");
        }
        _ = bundle_jh => {
            tracing::info!("bundle poller finished");
        }
        _ = simulator_jh => {
            tracing::info!("simulator finished");
        }
        _ = submit_jh => {
            tracing::info!("submit finished");
        },
        _ = server => {
            tracing::info!("server finished");
        }
    }

    tracing::info!("shutting down");

    Ok(())
}
