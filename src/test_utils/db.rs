//! Test database utilities for in-memory EVM state.
//! This module provides an in-memory database implementation that can be used
//! for testing block simulation without requiring network access.

use alloy::primitives::{Address, B256, U256};
use signet_sim::{AcctInfo, StateSource};
use std::time::Duration;
use trevm::revm::{
    bytecode::Bytecode,
    database::{CacheDB, EmptyDB},
    database_interface::DatabaseRef,
    primitives::{StorageKey, StorageValue},
    state::AccountInfo,
};

/// Mirrors the production `CacheDB<AlloyDB>` stack: the outer [`CacheDB`] starts
/// empty and caches on the mutable `Database` path, while the inner [`LatencyDb`]
/// holds all state in-memory and applies configurable sleep on every read to
/// simulate RPC round-trip cost.
pub type TestDb = CacheDB<LatencyDb>;

/// A [`StateSource`] for testing backed by an in-memory [`TestDb`].
/// Returns actual account info (nonce, balance) from the database,
/// which is required for preflight validity checks during simulation.
#[derive(Debug, Clone)]
pub struct TestStateSource {
    db: TestDb,
}

impl TestStateSource {
    /// Create a new `TestStateSource` backed by the given database.
    pub const fn new(db: TestDb) -> Self {
        Self { db }
    }
}

impl StateSource for TestStateSource {
    type Error = std::convert::Infallible;

    async fn account_details(&self, address: &Address) -> Result<AcctInfo, Self::Error> {
        match self.db.basic_ref(*address) {
            Ok(Some(info)) => Ok(AcctInfo {
                nonce: info.nonce,
                balance: info.balance,
                has_code: info.code_hash != trevm::revm::primitives::KECCAK_EMPTY,
            }),
            _ => Ok(AcctInfo { nonce: 0, balance: U256::ZERO, has_code: false }),
        }
    }
}

/// Builder for creating pre-populated test databases.
/// Use this builder to set up blockchain state (accounts, contracts, storage)
/// before running simulations.
#[derive(Debug)]
pub struct TestDbBuilder {
    latency_db: LatencyDb,
}

impl Default for TestDbBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestDbBuilder {
    /// Create a new empty test database builder.
    pub fn new() -> Self {
        Self { latency_db: LatencyDb::default() }
    }

    /// Add an account with the specified balance and nonce.
    ///
    /// # Arguments
    ///
    /// * `address` - The account address
    /// * `balance` - The account balance in wei
    /// * `nonce` - The account nonce (transaction count)
    pub fn with_account(mut self, address: Address, balance: U256, nonce: u64) -> Self {
        self.latency_db
            .in_mem_db
            .insert_account_info(address, AccountInfo { balance, nonce, ..Default::default() });
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
        if !self.latency_db.in_mem_db.cache.accounts.contains_key(&address) {
            self.latency_db.in_mem_db.insert_account_info(address, AccountInfo::default());
        }
        let _ = self.latency_db.in_mem_db.insert_account_storage(address, slot, value);
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
        self.latency_db.in_mem_db.cache.block_hashes.insert(U256::from(number), hash);
        self
    }

    /// Add a contract account with the specified bytecode.
    pub fn with_contract(mut self, address: Address, bytecode: Bytecode) -> Self {
        self.latency_db.in_mem_db.insert_account_info(
            address,
            AccountInfo { code: Some(bytecode), ..Default::default() },
        );
        self
    }

    /// Apply the given latency to every call to the wrapped, in-memory DB.
    pub const fn with_latency(mut self, latency: Duration) -> Self {
        self.latency_db.latency = Some(latency);
        self
    }

    /// Build the test database.
    pub fn build(self) -> TestDb {
        CacheDB::new(self.latency_db)
    }
}

/// In-memory database with configurable per-access latency.
///
/// Holds all state (accounts, storage, block hashes) in plain hash maps and
/// calls [`std::thread::sleep`] before every read through [`DatabaseRef`].
/// This simulates production conditions where the backing store (e.g. `AlloyDB`)
/// sends an RPC for each state lookup.
///
/// Intended to be wrapped in a [`CacheDB`] so that the `CacheDB` provides real
/// mutable-path caching while this type represents the slow backing store behind it.
#[derive(Debug, Clone, Default)]
pub struct LatencyDb {
    in_mem_db: CacheDB<EmptyDB>,
    latency: Option<Duration>,
}

impl LatencyDb {
    fn sleep(&self) {
        if let Some(latency) = self.latency {
            std::thread::sleep(latency);
        }
    }
}

impl DatabaseRef for LatencyDb {
    type Error = core::convert::Infallible;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.sleep();
        Ok(self.in_mem_db.basic_ref(address).unwrap())
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.sleep();
        Ok(self.in_mem_db.code_by_hash_ref(code_hash).unwrap())
    }

    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.sleep();
        Ok(self.in_mem_db.storage_ref(address, index).unwrap())
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.sleep();
        Ok(self.in_mem_db.block_hash_ref(number).unwrap())
    }
}
