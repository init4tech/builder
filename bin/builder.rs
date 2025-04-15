use builder::{
    config::BuilderConfig,
    service::serve_builder_with_span,
    tasks::{
        block::BlockBuilder, bundler, metrics::MetricsTask, oauth::Authenticator,
        submit::SubmitTask, tx_poller,
    },
};
use tokio::select;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let _guard = init4_bin_base::init4();

    let span = tracing::info_span!("zenith-builder");

    let config = BuilderConfig::load_from_env()?.clone();
    let host_provider = config.connect_host_provider().await?;
    let ru_provider = config.connect_ru_provider().await?;
    let authenticator = Authenticator::new(&config);

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

    let tx_poller = tx_poller::TxPoller::new(&config);
    let (tx_receiver, tx_poller_jh) = tx_poller.spawn();

    let bundle_poller = bundler::BundlePoller::new(&config, authenticator.clone());
    let (bundle_receiver, bundle_poller_jh) = bundle_poller.spawn();

    let authenticator_jh = authenticator.spawn();

    let (submit_channel, submit_jh) = submit.spawn();

    let build_jh = builder.spawn(ru_provider, tx_receiver, bundle_receiver, submit_channel);

    let port = config.builder_port;
    let server = serve_builder_with_span(([0, 0, 0, 0], port), span);

    select! {
        _ = tx_poller_jh => {
            tracing::info!("tx_poller finished");
        },
        _ = bundle_poller_jh => {
            tracing::info!("bundle_poller finished");
        },
        _ = submit_jh => {
            tracing::info!("submit finished");
        },
        _ = metrics_jh => {
            tracing::info!("metrics finished");
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
