//! Test utilities for block building and simulation.
//! This module provides builders for creating `BlockBuild` instances
//! for testing block simulation.

use super::{
    db::{TestDb, TestStateSource},
    env::{TestHostEnv, TestRollupEnv, TestSimEnvBuilder},
};
use signet_sim::{BlockBuild, BuiltBlock, SimCache};
use std::time::Duration;
use tokio::time::Instant;
use trevm::revm::inspector::NoOpInspector;

/// Test block builder type using in-memory databases.
pub type TestBlockBuild =
    BlockBuild<TestDb, TestDb, TestStateSource, TestStateSource, NoOpInspector, NoOpInspector>;

/// Builder for creating test `BlockBuild` instances.
/// Configures all the parameters needed for block simulation
/// and provides sensible defaults for testing scenarios.
#[derive(Debug)]
pub struct TestBlockBuildBuilder {
    /// The test environment configuration for the block build.
    env: TestBlockBuildEnv,
    /// The simulation cache to use for the block build.
    sim_cache: SimCache,
    /// The duration from now until the block build should finish.
    deadline_duration: Duration,
    /// The concurrency limit for parallel simulation.
    concurrency_limit: usize,
    /// The maximum gas limit for the rollup block.
    max_gas: u64,
    /// The maximum gas limit for host transactions.
    max_host_gas: u64,
}

/// Internal enum to manage the environment configuration for the block build.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum TestBlockBuildEnv {
    /// A builder that will create the environments when `into_block_build()` is called.
    Builder(TestSimEnvBuilder),
    /// A pair of already built environments to use directly.
    Built { rollup: TestRollupEnv, host: TestHostEnv },
}

impl Default for TestBlockBuildBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestBlockBuildBuilder {
    /// Create a new test block build builder with test-focused defaults.
    /// Default values:
    /// - Deadline: 2 seconds
    /// - Concurrency limit: 4
    /// - Max gas: 3,000,000,000 (3 billion)
    /// - Max host gas: 24,000,000
    pub fn new() -> Self {
        Self {
            // Default to building fresh test environments unless the caller injects a pair.
            env: TestBlockBuildEnv::Builder(TestSimEnvBuilder::new()),
            sim_cache: SimCache::new(),
            deadline_duration: Duration::from_secs(2),
            concurrency_limit: 4,
            max_gas: 3_000_000_000,
            max_host_gas: 24_000_000,
        }
    }

    /// Set the simulation environment builder.
    /// The environments will be built from this builder when `into_block_build()` is called.
    pub fn with_sim_env_builder(mut self, builder: TestSimEnvBuilder) -> Self {
        self.env = TestBlockBuildEnv::Builder(builder);
        self
    }

    /// Set both environments directly so the block build uses a consistent pair.
    pub fn with_envs(mut self, rollup: TestRollupEnv, host: TestHostEnv) -> Self {
        self.env = TestBlockBuildEnv::Built { rollup, host };
        self
    }

    /// Set the simulation cache.
    pub fn with_cache(mut self, cache: SimCache) -> Self {
        self.sim_cache = cache;
        self
    }

    /// Set the deadline duration from now.
    pub const fn with_deadline(mut self, duration: Duration) -> Self {
        self.deadline_duration = duration;
        self
    }

    /// Set the concurrency limit for parallel simulation.
    pub const fn with_concurrency(mut self, limit: usize) -> Self {
        self.concurrency_limit = limit;
        self
    }

    /// Set the maximum gas limit for the rollup block.
    pub const fn with_max_gas(mut self, gas: u64) -> Self {
        self.max_gas = gas;
        self
    }

    /// Set the maximum gas limit for host transactions.
    pub const fn with_max_host_gas(mut self, gas: u64) -> Self {
        self.max_host_gas = gas;
        self
    }

    /// Build the test `BlockBuild` instance.
    /// This creates a `BlockBuild` ready for simulation.
    /// Call `.build().await` on the result to execute the simulation and get a `BuiltBlock`.
    pub fn build(self) -> TestBlockBuild {
        let builder = self.sim_env_builder.unwrap_or_default();
        let ru_state_source = TestStateSource::new(builder.rollup_db());
        let host_state_source = TestStateSource::new(builder.host_db());

        let (rollup_env, host_env) = match (self.rollup_env, self.host_env) {
            (Some(rollup), Some(host)) => (rollup, host),
            _ => builder.build(),
        };

        // Convert the relative deadline into the absolute instant expected by `BlockBuild`.
        let finish_by = Instant::now() + self.deadline_duration;

        BlockBuild::new(
            rollup_env,
            host_env,
            finish_by,
            self.concurrency_limit,
            self.sim_cache,
            self.max_gas,
            self.max_host_gas,
            ru_state_source,
            host_state_source,
        )
        .build()
        .await
    }
}

/// Convenience function to quickly build a block with a cache and optional configuration.
/// This is useful for simple test cases where you just want to simulate
/// some transactions quickly.
pub async fn quick_build_block(cache: SimCache, deadline: Duration) -> BuiltBlock {
    TestBlockBuildBuilder::new().with_cache(cache).with_deadline(deadline).into_block_build().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_build_builder_defaults() {
        let builder = TestBlockBuildBuilder::new();
        assert_eq!(builder.deadline_duration, Duration::from_secs(2));
        assert_eq!(builder.concurrency_limit, 4);
        assert_eq!(builder.max_gas, 3_000_000_000);
        assert_eq!(builder.max_host_gas, 24_000_000);
    }

    #[test]
    fn test_block_build_builder_custom_values() {
        let builder = TestBlockBuildBuilder::new()
            .with_deadline(Duration::from_secs(5))
            .with_concurrency(8)
            .with_max_gas(1_000_000_000)
            .with_max_host_gas(10_000_000);

        assert_eq!(builder.deadline_duration, Duration::from_secs(5));
        assert_eq!(builder.concurrency_limit, 8);
        assert_eq!(builder.max_gas, 1_000_000_000);
        assert_eq!(builder.max_host_gas, 10_000_000);
    }
}
