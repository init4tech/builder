//! Benchmarks for complex bundle shapes: multi-tx bundles and bundles with host transactions.

use crate::fixture::{Fixture, set_up_host_tx_bundle_sim, set_up_multi_tx_bundle_sim};
use builder::test_utils::setup_test_config;
use criterion::{BatchSize, BenchmarkId, Criterion};

pub fn bench_multi_tx_bundles(criterion: &mut Criterion) {
    const BUNDLE_COUNT: usize = 100;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group = criterion.benchmark_group(format!(
        "sim_{BUNDLE_COUNT}_bundles_no_db_latency_varying_txs_per_bundle"
    ));
    group.sample_size(10);

    for txs_per_bundle in [1, 3, 5, 10] {
        group.bench_with_input(
            BenchmarkId::new("txs_per_bundle", txs_per_bundle),
            &txs_per_bundle,
            |bench, &tpb| {
                bench.to_async(&runtime).iter_batched(
                    || set_up_multi_tx_bundle_sim(BUNDLE_COUNT, tpb, CONCURRENCY),
                    Fixture::run,
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

pub fn bench_host_tx_bundles(criterion: &mut Criterion) {
    const BUNDLE_COUNT: usize = 100;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group = criterion.benchmark_group(format!(
        "sim_{BUNDLE_COUNT}_bundles_no_db_latency_varying_host_txs_per_bundle"
    ));
    group.sample_size(10);

    for host_txs in [0, 1, 3, 5] {
        group.bench_with_input(
            BenchmarkId::new("host_txs_per_bundle", host_txs),
            &host_txs,
            |bench, &htpb| {
                bench.to_async(&runtime).iter_batched(
                    || set_up_host_tx_bundle_sim(BUNDLE_COUNT, htpb, CONCURRENCY),
                    Fixture::run,
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}
