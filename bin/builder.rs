use builder::{
    config::BuilderConfig,
    service::serve_builder_with_span,
    tasks::{
        block::Simulator, bundler, metrics::MetricsTask, oauth::Authenticator, submit::SubmitTask,
        tx_poller,
    },
};
use signet_sim::SimCache;
use signet_types::SlotCalculator;
use std::sync::Arc;
use tokio::select;

// Note: Must be set to `multi_thread` to support async tasks.
// See: https://docs.rs/tokio/latest/tokio/attr.main.html
#[tokio::main(flavor = "multi_thread")]
async fn main() -> eyre::Result<()> {
    let _guard = init4_bin_base::init4();

    let span = tracing::info_span!("zenith-builder");

    let config = BuilderConfig::load_from_env()?.clone();
    let constants = config.load_pecorino_constants();
    let authenticator = Authenticator::new(&config);

    let (host_provider, ru_provider, sequencer_signer) = tokio::try_join!(
        config.connect_host_provider(),
        config.connect_ru_provider(),
        config.connect_sequencer_signer(),
    )?;

    let zenith = config.connect_zenith(host_provider.clone());

    let metrics = MetricsTask { host_provider: host_provider.clone() };
    let (tx_channel, metrics_jh) = metrics.spawn();

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

    let sim_items = SimCache::new();
    let slot_calculator =
        SlotCalculator::new(config.start_timestamp, config.chain_offset, config.target_slot_time);

    let sim = Arc::new(Simulator::new(&config, ru_provider.clone(), slot_calculator));

    let (basefee_jh, sim_cache_jh) =
        sim.clone().spawn_cache_tasks(tx_receiver, bundle_receiver, sim_items.clone());

    let build_jh = sim.clone().spawn_simulator_task(constants, sim_items.clone(), submit_channel);

    let port = config.builder_port;
    let server = serve_builder_with_span(([0, 0, 0, 0], port), span);

    select! {
        _ = tx_poller_jh => {
            tracing::info!("tx_poller finished");
        },
        _ = bundle_poller_jh => {
            tracing::info!("bundle_poller finished");
        },
        _ = sim_cache_jh => {
            tracing::info!("sim cache task finished");
        }
        _ = basefee_jh => {
            tracing::info!("basefee task finished");
        }
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
