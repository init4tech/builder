//! Test simulation environment utilities.
//! This module provides builders for creating `RollupEnv` and `HostEnv`
//! instances with in-memory databases for offline testing.

use super::db::{TestDb, TestDbBuilder};
use crate::tasks::block::cfg::SignetCfgEnv;
use alloy::primitives::{Address, B256, U256};
use signet_constants::SignetSystemConstants;
use signet_sim::{HostEnv, RollupEnv};
use trevm::revm::{context::BlockEnv, context_interface::block::BlobExcessGasAndPrice, inspector::NoOpInspector};

/// Test rollup environment using in-memory database.
pub type TestRollupEnv = RollupEnv<TestDb, NoOpInspector>;

/// Test host environment using in-memory database.
pub type TestHostEnv = HostEnv<TestDb, NoOpInspector>;

/// Builder for creating test simulation environments.
/// This allows you to configure both rollup and host environments
/// with pre-populated in-memory databases for testing block simulation
/// without network access.
#[derive(Debug)]
pub struct TestSimEnvBuilder {
    rollup_db: TestDb,
    host_db: TestDb,
    rollup_block_env: BlockEnv,
    host_block_env: BlockEnv,
    constants: SignetSystemConstants,
}

impl Default for TestSimEnvBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestSimEnvBuilder {
    /// Create a new builder with default Parmigiana constants and empty databases.
    pub fn new() -> Self {
        let default_block_env = Self::default_block_env();
        Self {
            rollup_db: TestDbBuilder::new().build(),
            host_db: TestDbBuilder::new().build(),
            rollup_block_env: default_block_env.clone(),
            host_block_env: default_block_env,
            constants: SignetSystemConstants::parmigiana(),
        }
    }

    /// Create a default block environment suitable for testing.
    fn default_block_env() -> BlockEnv {
        BlockEnv {
            number: U256::from(100u64),
            beneficiary: Address::repeat_byte(0x01),
            timestamp: U256::from(1700000000u64),
            gas_limit: 3_000_000_000,
            basefee: 1_000_000_000, // 1 gwei
            difficulty: U256::ZERO,
            prevrandao: Some(B256::random()),
            blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
                excess_blob_gas: 0,
                blob_gasprice: 0,
            }),
        }
    }

    /// Set the rollup database.
    pub fn with_rollup_db(mut self, db: TestDb) -> Self {
        self.rollup_db = db;
        self
    }

    /// Set the host database.
    pub fn with_host_db(mut self, db: TestDb) -> Self {
        self.host_db = db;
        self
    }

    /// Set the rollup block environment.
    pub fn with_rollup_block_env(mut self, env: BlockEnv) -> Self {
        self.rollup_block_env = env;
        self
    }

    /// Set the host block environment.
    pub fn with_host_block_env(mut self, env: BlockEnv) -> Self {
        self.host_block_env = env;
        self
    }

    /// Set both rollup and host block environments to the same value.
    pub fn with_block_env(mut self, env: BlockEnv) -> Self {
        self.rollup_block_env = env.clone();
        self.host_block_env = env;
        self
    }

    /// Set the system constants.
    pub fn with_constants(mut self, constants: SignetSystemConstants) -> Self {
        self.constants = constants;
        self
    }

    /// Build the test RollupEnv.
    pub fn build_rollup_env(&self) -> TestRollupEnv {
        let timestamp = self.rollup_block_env.timestamp.to::<u64>();
        let cfg = SignetCfgEnv::new(self.constants.ru_chain_id(), timestamp);
        RollupEnv::new(
            self.rollup_db.clone(),
            self.constants.clone(),
            &cfg,
            &self.rollup_block_env,
        )
    }

    /// Build the test HostEnv.
    pub fn build_host_env(&self) -> TestHostEnv {
        let timestamp = self.host_block_env.timestamp.to::<u64>();
        let cfg = SignetCfgEnv::new(self.constants.host_chain_id(), timestamp);
        HostEnv::new(
            self.host_db.clone(),
            self.constants.clone(),
            &cfg,
            &self.host_block_env,
        )
    }

    /// Build both environments as a tuple.
    pub fn build(self) -> (TestRollupEnv, TestHostEnv) {
        let rollup = self.build_rollup_env();
        let host = self.build_host_env();
        (rollup, host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sim_env_builder_creates_environments() {
        let builder = TestSimEnvBuilder::new();
        let (rollup_env, host_env) = builder.build();

        // Verify environments were created (we can't easily inspect internals,
        // but we can verify they don't panic during creation)
        let _ = rollup_env;
        let _ = host_env;
    }

    #[test]
    fn test_sim_env_builder_with_custom_block_env() {
        let custom_env = BlockEnv {
            number: U256::from(500u64),
            beneficiary: Address::repeat_byte(0x42),
            timestamp: U256::from(1800000000u64),
            gas_limit: 5_000_000_000,
            basefee: 2_000_000_000,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::repeat_byte(0x11)),
            blob_excess_gas_and_price: None,
        };

        let builder = TestSimEnvBuilder::new().with_block_env(custom_env);
        let (rollup_env, host_env) = builder.build();

        let _ = rollup_env;
        let _ = host_env;
    }
}
