#![allow(dead_code)]

use builder::config::BuilderConfig;
use builder::service::serve_builder_with_span;
use builder::tasks::block::BlockBuilder;
use builder::tasks::bundler::BundlePoller;
use builder::tasks::metrics::MetricsTask;
use builder::tasks::oauth::Authenticator;
use builder::tasks::simulator::eval_fn;
use builder::tasks::submit::SubmitTask;

use builder::tasks::tx_poller::{self, TxPoller};
use revm::primitives::U256;
use tokio::select;
use trevm::revm::primitives::ResultAndState;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let _guard = init4_bin_base::init4();

    let span = tracing::info_span!("zenith-builder");

    let config = BuilderConfig::load_from_env()?.clone();

    let host_provider = config.connect_host_provider().await?;
    let ru_provider = config.connect_ru_provider().await?;
    tracing::info!(rpc_url = config.host_rpc_url.as_ref(), "instantiated provider");

    let sequencer_signer = config.connect_sequencer_signer().await?;
    let zenith = config.connect_zenith(host_provider.clone());
    tracing::info!("instantiated zenith");

    let metrics = MetricsTask { host_provider: host_provider.clone() };
    let (tx_channel, metrics_jh) = metrics.spawn();
    tracing::info!("instantiated zenith");

    let authenticator = Authenticator::new(&config);
    let authenticator_jh = authenticator.spawn();
    tracing::info!("instantiated authenticator");

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
    
    let tx_poller = TxPoller::new_with_poll_interval_ms(&config, 1000);
    let (tx_channel, tx_jh) = tx_poller.spawn();

    let bundle_poller = BundlePoller::new_with_poll_interval_ms(&config, authenticator, 1);
    let bundle_channel = bundle_poller.spawn();

    let (submit_channel, submit_jh) = submit.spawn();
    let build_jh = builder.spawn(tx_channel, bundle_channel, submit_channel);

    let port = config.builder_port;
    let server = serve_builder_with_span(([0, 0, 0, 0], port), span);

    select! {
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
        _ = tx_jh => {
            tracing::info!("tx-poller finished");
        }
    }

    tracing::info!("shutting down");

    Ok(())
}
