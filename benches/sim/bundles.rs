//! Benchmarks for bundle simulation: varying counts, DB latency, and concurrency.

use crate::fixture::{Fixture, LATENCIES, set_up_bundle_sim, set_up_sender_distribution_sim};
use builder::test_utils::setup_test_config;
use criterion::{BatchSize, Bencher, BenchmarkId, Criterion};
use std::time::Duration;

pub fn bench_bundle_counts(criterion: &mut Criterion) {
    const LATENCY: Duration = Duration::ZERO;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group = criterion.benchmark_group("sim_varying_simple_bundle_counts_no_db_latency");
    group.sample_size(10);

    for bundle_count in [1, 10, 100, 1000] {
        group.bench_with_input(
            BenchmarkId::new("bundle_count", bundle_count),
            &bundle_count,
            |bench: &mut Bencher, &bundle_count| {
                bench.to_async(&runtime).iter_batched(
                    || set_up_bundle_sim(bundle_count, LATENCY, CONCURRENCY),
                    Fixture::run,
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

pub fn bench_bundle_db_latency(criterion: &mut Criterion) {
    const BUNDLE_COUNT: usize = 10;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group =
        criterion.benchmark_group(format!("sim_{BUNDLE_COUNT}_simple_bundles_varying_db_latency"));
    group.sample_size(10);

    for (label, latency) in LATENCIES {
        group.bench_with_input(BenchmarkId::new("latency", label), &latency, |bench, &lat| {
            bench.to_async(&runtime).iter_batched(
                || set_up_bundle_sim(BUNDLE_COUNT, lat, CONCURRENCY),
                Fixture::run,
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

pub fn bench_bundle_concurrency(criterion: &mut Criterion) {
    const BUNDLE_COUNT: usize = 100;
    const LATENCY: Duration = Duration::ZERO;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group = criterion.benchmark_group(format!(
        "sim_{BUNDLE_COUNT}_simple_bundles_no_db_latency_varying_concurrency"
    ));
    group.sample_size(10);

    for concurrency in [1, 2, 4, 8, 16] {
        group.bench_with_input(
            BenchmarkId::new("thread_count", concurrency),
            &concurrency,
            |bench, &conc| {
                bench.to_async(&runtime).iter_batched(
                    || set_up_bundle_sim(BUNDLE_COUNT, LATENCY, conc),
                    Fixture::run,
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}

pub fn bench_bundle_sender_distribution(criterion: &mut Criterion) {
    const BUNDLE_COUNT: usize = 100;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group = criterion.benchmark_group(format!(
        "sim_{BUNDLE_COUNT}_simple_bundles_no_db_latency_varying_sender_count"
    ));
    group.sample_size(10);

    for num_senders in [1, 5, 20, 100] {
        group.bench_with_input(
            BenchmarkId::new("sender_count", num_senders),
            &num_senders,
            |bench, &senders| {
                bench.to_async(&runtime).iter_batched(
                    || set_up_sender_distribution_sim(BUNDLE_COUNT, senders, CONCURRENCY),
                    Fixture::run,
                    BatchSize::PerIteration,
                );
            },
        );
    }
    group.finish();
}
