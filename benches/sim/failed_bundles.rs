//! Benchmarks for bundles that fail: preflight validation failures and EVM execution reverts.

use crate::fixture::{
    Fixture, LATENCIES, set_up_execution_failure_sim, set_up_preflight_failure_sim,
};
use builder::test_utils::setup_test_config;
use criterion::{BatchSize, BenchmarkId, Criterion};

pub fn bench_preflight_failure(criterion: &mut Criterion) {
    const COUNT: usize = 10;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group =
        criterion.benchmark_group(format!("sim_{COUNT}_simple_bundles_all_failing_preflight"));
    group.sample_size(10);

    for (label, latency) in LATENCIES {
        group.bench_with_input(BenchmarkId::new("latency", label), &latency, |bench, &lat| {
            bench.to_async(&runtime).iter_batched(
                || set_up_preflight_failure_sim(COUNT, lat, CONCURRENCY),
                Fixture::run,
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

pub fn bench_execution_failure(criterion: &mut Criterion) {
    const COUNT: usize = 10;
    const CONCURRENCY: usize = 4;

    setup_test_config();
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mut group =
        criterion.benchmark_group(format!("sim_{COUNT}_simple_bundles_all_failing_execution"));
    group.sample_size(10);

    for (label, latency) in LATENCIES {
        group.bench_with_input(BenchmarkId::new("latency", label), &latency, |bench, &lat| {
            bench.to_async(&runtime).iter_batched(
                || set_up_execution_failure_sim(COUNT, lat, CONCURRENCY),
                Fixture::run,
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}
