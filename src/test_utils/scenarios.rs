//! Pre-configured test scenarios.
//! This module provides ready-to-use test scenarios that configure databases,
//! environments, and builders for common testing needs.

use super::{
    block::TestBlockBuildBuilder, db::TestDbBuilder, env::TestSimEnvBuilder, tx::TestAccounts,
};
use alloy::primitives::{Address, B256, U256};
use signet_sim::SimCache;
use trevm::revm::{context::BlockEnv, context_interface::block::BlobExcessGasAndPrice};

/// Default test balance: 100 ETH in wei.
pub const DEFAULT_BALANCE: u128 = 100_000_000_000_000_000_000;

/// Default basefee: 1 gwei.
pub const DEFAULT_BASEFEE: u64 = 1_000_000_000;

/// Create a test database with pre-funded accounts.
///
/// # Arguments
///
/// * `accounts` - The test accounts to fund
/// * `balance` - The balance to give each account
pub fn funded_test_db(accounts: &TestAccounts, balance: U256) -> TestDbBuilder {
    TestDbBuilder::new()
        .with_account(accounts.alice_address(), balance, 0)
        .with_account(accounts.bob_address(), balance, 0)
        .with_account(accounts.charlie_address(), balance, 0)
}

/// Create a standard test block environment.
///
/// # Arguments
///
/// * `block_number` - The block number
/// * `basefee` - The base fee per gas
/// * `timestamp` - The block timestamp
/// * `gas_limit` - The block gas limit
pub fn test_block_env(block_number: u64, basefee: u64, timestamp: u64, gas_limit: u64) -> BlockEnv {
    BlockEnv {
        number: U256::from(block_number),
        beneficiary: Address::repeat_byte(0x01),
        timestamp: U256::from(timestamp),
        gas_limit,
        basefee,
        difficulty: U256::ZERO,
        prevrandao: Some(B256::random()),
        blob_excess_gas_and_price: Some(BlobExcessGasAndPrice {
            excess_blob_gas: 0,
            blob_gasprice: 0,
        }),
    }
}

/// Basic multi-transaction block building scenario.
///
/// Creates a scenario with:
/// - Three pre-funded test accounts (100 ETH each)
/// - Default block environment
/// - Empty SimCache ready for transactions
///
/// # Returns
///
/// A tuple of (builder, accounts, cache) ready for adding transactions and building.
pub fn basic_scenario() -> (TestBlockBuildBuilder, TestAccounts, SimCache) {
    let accounts = TestAccounts::new();
    let balance = U256::from(DEFAULT_BALANCE);

    let rollup_db = funded_test_db(&accounts, balance).build();
    let host_db = funded_test_db(&accounts, balance).build();

    let block_env = test_block_env(100, DEFAULT_BASEFEE, 1700000000, 3_000_000_000);

    let sim_env = TestSimEnvBuilder::new()
        .with_rollup_db(rollup_db)
        .with_host_db(host_db)
        .with_block_env(block_env);

    let cache = SimCache::new();

    let builder = TestBlockBuildBuilder::new().with_sim_env_builder(sim_env);

    (builder, accounts, cache)
}

/// Scenario for testing transaction priority ordering.
///
/// Similar to `basic_scenario` but with a longer deadline to ensure
/// all transactions can be processed and ordered.
///
/// # Returns
///
/// A tuple of (builder, accounts, cache) configured for priority testing.
pub fn priority_ordering_scenario() -> (TestBlockBuildBuilder, TestAccounts, SimCache) {
    let (builder, accounts, cache) = basic_scenario();

    // Use a longer deadline for ordering tests
    let builder = builder.with_deadline(std::time::Duration::from_secs(5));

    (builder, accounts, cache)
}

/// Scenario for testing gas limit enforcement.
///
/// Creates a scenario with a configurable gas limit to test that
/// block building respects the maximum gas constraint.
///
/// # Arguments
///
/// * `max_gas` - The maximum gas limit for the block
///
/// # Returns
///
/// A tuple of (builder, accounts, cache) with the specified gas limit.
pub fn gas_limit_scenario(max_gas: u64) -> (TestBlockBuildBuilder, TestAccounts, SimCache) {
    let (builder, accounts, cache) = basic_scenario();

    let builder = builder.with_max_gas(max_gas);

    (builder, accounts, cache)
}

/// Scenario with custom funded addresses.
///
/// Use this when you need specific addresses to be funded
/// (e.g., for testing contract interactions).
///
/// # Arguments
///
/// * `addresses` - List of (address, balance, nonce) tuples to fund
///
/// # Returns
///
/// A TestBlockBuildBuilder configured with the funded accounts.
pub fn custom_funded_scenario(
    addresses: &[(Address, U256, u64)],
) -> (TestBlockBuildBuilder, SimCache) {
    let mut db_builder = TestDbBuilder::new();

    for (addr, balance, nonce) in addresses {
        db_builder = db_builder.with_account(*addr, *balance, *nonce);
    }

    let db = db_builder.build();

    let sim_env = TestSimEnvBuilder::new().with_rollup_db(db.clone()).with_host_db(db);

    let cache = SimCache::new();
    let builder = TestBlockBuildBuilder::new().with_sim_env_builder(sim_env);

    (builder, cache)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_scenario_creates_funded_accounts() {
        let (_, accounts, _) = basic_scenario();

        // Verify accounts are created with unique addresses
        let addresses = accounts.addresses();
        assert_eq!(addresses.len(), 3);
        assert_ne!(addresses[0], addresses[1]);
    }

    #[test]
    fn test_gas_limit_scenario_creates_builder() {
        // Verify gas limit scenario creates a valid builder
        let (_, _, _) = gas_limit_scenario(100_000);
        // Builder creation succeeds (gas limit is applied internally)
    }

    #[test]
    fn test_custom_funded_scenario() {
        let addr1 = Address::repeat_byte(0x11);
        let addr2 = Address::repeat_byte(0x22);

        let (_, _) = custom_funded_scenario(&[
            (addr1, U256::from(1000u64), 5),
            (addr2, U256::from(2000u64), 10),
        ]);

        // Scenario creation succeeds
    }
}
