#![allow(dead_code)]

use builder::config::BuilderConfig;
use builder::otlp::{OtelConfig, OtelGuard};
use builder::service::serve_builder_with_span;
use builder::tasks::block::BlockBuilder;
use builder::tasks::metrics::MetricsTask;
use builder::tasks::oauth::Authenticator;
use builder::tasks::submit::SubmitTask;

use metrics_exporter_prometheus::PrometheusBuilder;
use tokio::select;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn init_tracing() -> Option<OtelGuard> {
    let registry = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer());

    if let Some(cfg) = OtelConfig::load() {
        let guard = cfg.provider();
        registry.with(guard.layer()).init();
        Some(guard)
    } else {
        registry.init();
        None
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let _guard = init_tracing();

    let span = tracing::info_span!("zenith-builder");

    let config = BuilderConfig::load_from_env()?.clone();
    let host_provider = config.connect_host_provider().await?;
    let ru_provider = config.connect_ru_provider().await?;
    let authenticator = Authenticator::new(&config);

    PrometheusBuilder::new().install().expect("failed to install prometheus exporter");

    tracing::debug!(rpc_url = config.host_rpc_url.as_ref(), "instantiated provider");

    let sequencer_signer = config.connect_sequencer_signer().await?;
    let zenith = config.connect_zenith(host_provider.clone());

    let metrics = MetricsTask { host_provider: host_provider.clone() };
    let (tx_channel, metrics_jh) = metrics.spawn();

    let builder = BlockBuilder::new(&config, authenticator.clone(), ru_provider.clone());
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
            tracing::info!(slang_definition = r#"in this context, "cooked" means that it finished running"#, "submit is cooked");
        },
        _ = metrics_jh => {
            tracing::info!(slang_definition = r#"in this context, "cooked" means that it finished running"#, "metrics is cooked");
        },
        _ = build_jh => {
            tracing::info!(slang_definition = r#"in this context, "cooked" means that it finished running"#, "build is cooked");
        }
        _ = server => {
            tracing::info!(slang_definition = r#"in this context, "cooked" means that it finished running"#, "server is cooked");
        }
        _ = authenticator_jh => {
            tracing::info!(slang_definition = r#"in this context, "cooked" means that it finished running"#, "authenticator is cooked");
        }
    }

    tracing::info!("shutting down");

    Ok(())
}
