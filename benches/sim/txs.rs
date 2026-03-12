//! Benchmarks for standalone transaction simulation: varying counts and DB latency.

use crate::fixture::{Fixture, LATENCIES, set_up_tx_sim};
use builder::test_utils::setup_test_config;
use criterion::{BatchSize, Bencher, BenchmarkId, Criterion};
use std::time::Duration;

pub fn bench_tx_counts(criterion: &mut Criterion) {
    const LATENCY: Duration = Duration::ZERO;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group = criterion.benchmark_group("sim_varying_tx_counts_with_no_db_latency");
    group.sample_size(10);

    for tx_count in [1, 10, 100, 1000] {
        group.bench_with_input(
            BenchmarkId::new("tx_count", tx_count),
            &tx_count,
            |bench: &mut Bencher, &tx_count| {
                bench.to_async(&runtime).iter_batched(
                    || set_up_tx_sim(tx_count, LATENCY, CONCURRENCY),
                    Fixture::run,
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

pub fn bench_tx_db_latency(criterion: &mut Criterion) {
    const TX_COUNT: usize = 10;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group =
        criterion.benchmark_group(format!("sim_{TX_COUNT}_txs_with_varying_db_latency"));
    group.sample_size(10);

    for (label, latency) in LATENCIES {
        group.bench_with_input(BenchmarkId::new("latency", label), &latency, |bench, &lat| {
            bench.to_async(&runtime).iter_batched(
                || set_up_tx_sim(TX_COUNT, lat, CONCURRENCY),
                Fixture::run,
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}
