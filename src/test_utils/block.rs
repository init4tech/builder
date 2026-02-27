//! Test utilities for block building and simulation.
//! This module provides builders for creating `BlockBuild` instances
//! for testing block simulation.

use super::{
    db::TestDb,
    env::{TestHostEnv, TestRollupEnv, TestSimEnvBuilder},
};
use signet_sim::{BlockBuild, BuiltBlock, SimCache};
use std::time::Duration;
use tokio::time::Instant;
use trevm::revm::inspector::NoOpInspector;

/// Test block builder type using in-memory databases.
pub type TestBlockBuild = BlockBuild<TestDb, TestDb, NoOpInspector, NoOpInspector>;

/// Builder for creating test `BlockBuild` instances.
/// Configures all the parameters needed for block simulation
/// and provides sensible defaults for testing scenarios.
#[derive(Debug)]
pub struct TestBlockBuildBuilder {
    rollup_env: Option<TestRollupEnv>,
    host_env: Option<TestHostEnv>,
    sim_env_builder: Option<TestSimEnvBuilder>,
    sim_cache: SimCache,
    deadline_duration: Duration,
    concurrency_limit: usize,
    max_gas: u64,
    max_host_gas: u64,
}

impl Default for TestBlockBuildBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestBlockBuildBuilder {
    /// Create a new test block build builder with sensible defaults.
    /// Default values:
    /// - Deadline: 2 seconds
    /// - Concurrency limit: 4
    /// - Max gas: 3,000,000,000 (3 billion)
    /// - Max host gas: 24,000,000
    pub fn new() -> Self {
        Self {
            rollup_env: None,
            host_env: None,
            sim_env_builder: Some(TestSimEnvBuilder::new()),
            sim_cache: SimCache::new(),
            deadline_duration: Duration::from_secs(2),
            concurrency_limit: 4,
            max_gas: 3_000_000_000,
            max_host_gas: 24_000_000,
        }
    }

    /// Set the simulation environment builder.
    /// The environments will be built from this builder when `build()` is called.
    pub fn with_sim_env_builder(mut self, builder: TestSimEnvBuilder) -> Self {
        self.sim_env_builder = Some(builder);
        self.rollup_env = None;
        self.host_env = None;
        self
    }

    /// Set the rollup environment directly.
    pub fn with_rollup_env(mut self, env: TestRollupEnv) -> Self {
        self.rollup_env = Some(env);
        self
    }

    /// Set the host environment directly.
    pub fn with_host_env(mut self, env: TestHostEnv) -> Self {
        self.host_env = Some(env);
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
        let (rollup_env, host_env) = match (self.rollup_env, self.host_env) {
            (Some(rollup), Some(host)) => (rollup, host),
            _ => {
                let builder = self.sim_env_builder.unwrap_or_default();
                builder.build()
            }
        };

        let finish_by = Instant::now() + self.deadline_duration;

        BlockBuild::new(
            rollup_env,
            host_env,
            finish_by,
            self.concurrency_limit,
            self.sim_cache,
            self.max_gas,
            self.max_host_gas,
        )
    }
}

/// Convenience function to quickly build a block with a cache and optional configuration.
/// This is useful for simple test cases where you just want to simulate
/// some transactions quickly.
pub async fn quick_build_block(cache: SimCache, deadline: Duration) -> BuiltBlock {
    TestBlockBuildBuilder::new().with_cache(cache).with_deadline(deadline).build().build().await
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
