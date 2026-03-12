mod bundles;
mod complex_bundles;
mod failed_bundles;
mod fixture;
mod txs;

use criterion::{criterion_group, criterion_main};

criterion_group!(
    benches,
    bundles::bench_bundle_counts,
    bundles::bench_bundle_db_latency,
    bundles::bench_bundle_concurrency,
    bundles::bench_bundle_sender_distribution,
    complex_bundles::bench_multi_tx_bundles,
    complex_bundles::bench_host_tx_bundles,
    txs::bench_tx_counts,
    txs::bench_tx_db_latency,
    failed_bundles::bench_preflight_failure,
    failed_bundles::bench_execution_failure,
);
criterion_main!(benches);
