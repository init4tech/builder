//! Test database utilities for in-memory EVM state.
//! This module provides an in-memory database implementation that can be used
//! for testing block simulation without requiring network access.

use alloy::primitives::{Address, B256, U256};
use trevm::revm::{
    database::{CacheDB, EmptyDB},
    state::AccountInfo,
};

/// In-memory database for testing (no network access required).
/// This is a type alias for revm's `CacheDB<EmptyDB>`, which stores all
/// blockchain state in memory. It implements `DatabaseRef` and can be used
/// with `RollupEnv` and `HostEnv` for offline simulation testing.
pub type TestDb = CacheDB<EmptyDB>;

/// Builder for creating pre-populated test databases.
/// Use this builder to set up blockchain state (accounts, contracts, storage)
/// before running simulations.
#[derive(Debug)]
pub struct TestDbBuilder {
    db: TestDb,
}

impl Default for TestDbBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestDbBuilder {
    /// Create a new empty test database builder.
    pub fn new() -> Self {
        Self {
            db: CacheDB::new(EmptyDB::default()),
        }
    }

    /// Add an account with the specified balance and nonce.
    ///
    /// # Arguments
    ///
    /// * `address` - The account address
    /// * `balance` - The account balance in wei
    /// * `nonce` - The account nonce (transaction count)
    pub fn with_account(mut self, address: Address, balance: U256, nonce: u64) -> Self {
        self.db.insert_account_info(
            address,
            AccountInfo {
                balance,
                nonce,
                ..Default::default()
            },
        );
        self
    }

    /// Set a storage slot for an account.
    ///
    /// # Arguments
    ///
    /// * `address` - The account address
    /// * `slot` - The storage slot index
    /// * `value` - The value to store
    pub fn with_storage(mut self, address: Address, slot: U256, value: U256) -> Self {
        // Ensure the account exists before setting storage
        if self.db.cache.accounts.get(&address).is_none() {
            self.db.insert_account_info(address, AccountInfo::default());
        }
        let _ = self.db.insert_account_storage(address, slot, value);
        self
    }

    /// Insert a block hash for a specific block number.
    ///
    /// This is useful for testing contracts that use the BLOCKHASH opcode.
    ///
    /// # Arguments
    ///
    /// * `number` - The block number
    /// * `hash` - The block hash
    pub fn with_block_hash(mut self, number: u64, hash: B256) -> Self {
        self.db.cache.block_hashes.insert(U256::from(number), hash);
        self
    }

    /// Build the test database.
    pub fn build(self) -> TestDb {
        self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_builder_creates_empty_db() {
        let db = TestDbBuilder::new().build();
        assert!(db.cache.accounts.is_empty());
    }

    #[test]
    fn test_db_builder_adds_account() {
        let address = Address::repeat_byte(0x01);
        let balance = U256::from(1000u64);
        let nonce = 5u64;

        let db = TestDbBuilder::new()
            .with_account(address, balance, nonce)
            .build();

        let account = db.cache.accounts.get(&address).unwrap();
        assert_eq!(account.info.balance, balance);
        assert_eq!(account.info.nonce, nonce);
    }

    #[test]
    fn test_db_builder_adds_storage() {
        let address = Address::repeat_byte(0x01);
        let slot = U256::from(42u64);
        let value = U256::from(123u64);

        let db = TestDbBuilder::new()
            .with_storage(address, slot, value)
            .build();

        let account = db.cache.accounts.get(&address).unwrap();
        let stored = account.storage.get(&slot).unwrap();
        assert_eq!(*stored, value);
    }

    #[test]
    fn test_db_builder_adds_block_hash() {
        let number = 100u64;
        let hash = B256::repeat_byte(0xab);

        let db = TestDbBuilder::new().with_block_hash(number, hash).build();

        let stored = db.cache.block_hashes.get(&U256::from(number)).unwrap();
        assert_eq!(*stored, hash);
    }
}
