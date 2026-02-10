//! Test transaction utilities.
//! This module provides helpers for creating test transactions and accounts
//! for use in simulation tests.

use alloy::{
    consensus::{
        SignableTransaction, TxEip1559, TxEnvelope, transaction::Recovered,
        transaction::SignerRecoverable,
    },
    primitives::{Address, TxKind, U256},
    signers::{SignerSync, local::PrivateKeySigner},
};
use eyre::Result;

/// Pre-funded test accounts for simulation testing.
/// These accounts can be used to create and sign test transactions.
/// Use `TestDbBuilder` to fund these accounts in the test database.
#[derive(Debug, Clone)]
pub struct TestAccounts {
    /// First test account (Alice)
    pub alice: PrivateKeySigner,
    /// Second test account (Bob)
    pub bob: PrivateKeySigner,
    /// Third test account (Charlie)
    pub charlie: PrivateKeySigner,
}

impl Default for TestAccounts {
    fn default() -> Self {
        Self::new()
    }
}

impl TestAccounts {
    /// Create new random test accounts.
    pub fn new() -> Self {
        Self {
            alice: PrivateKeySigner::random(),
            bob: PrivateKeySigner::random(),
            charlie: PrivateKeySigner::random(),
        }
    }

    /// Get all account addresses.
    pub fn addresses(&self) -> Vec<Address> {
        vec![self.alice.address(), self.bob.address(), self.charlie.address()]
    }

    /// Get Alice's address.
    pub const fn alice_address(&self) -> Address {
        self.alice.address()
    }

    /// Get Bob's address.
    pub const fn bob_address(&self) -> Address {
        self.bob.address()
    }

    /// Get Charlie's address.
    pub const fn charlie_address(&self) -> Address {
        self.charlie.address()
    }
}

/// Create a signed EIP-1559 transfer transaction.
///
/// # Arguments
///
/// * `signer` - The account signing the transaction
/// * `to` - The recipient address
/// * `value` - The amount of wei to transfer
/// * `nonce` - The sender's nonce (transaction count)
/// * `chain_id` - The chain ID (e.g., signet_constants::parmigiana::RU_CHAIN_ID)
/// * `max_priority_fee_per_gas` - The priority fee (tip) per gas unit
///
/// # Returns
///
/// A recovered transaction envelope ready to be added to a SimCache.
pub fn create_transfer_tx(
    signer: &PrivateKeySigner,
    to: Address,
    value: U256,
    nonce: u64,
    chain_id: u64,
    max_priority_fee_per_gas: u128,
) -> Result<Recovered<TxEnvelope>> {
    let tx = TxEip1559 {
        chain_id,
        nonce,
        max_fee_per_gas: 100_000_000_000, // 100 gwei max fee
        max_priority_fee_per_gas,
        gas_limit: 21_000, // Standard transfer gas
        to: TxKind::Call(to),
        value,
        ..Default::default()
    };

    let signature = signer.sign_hash_sync(&tx.signature_hash())?;
    let signed = TxEnvelope::Eip1559(tx.into_signed(signature));
    let recovered = signed.try_into_recovered()?;
    Ok(recovered)
}

/// Create a signed EIP-1559 contract call transaction.
///
/// # Arguments
///
/// * `signer` - The account signing the transaction
/// * `to` - The contract address
/// * `input` - The calldata for the contract call
/// * `value` - The amount of wei to send with the call
/// * `nonce` - The sender's nonce
/// * `chain_id` - The chain ID
/// * `gas_limit` - The gas limit for the transaction
/// * `max_priority_fee_per_gas` - The priority fee per gas unit
#[allow(clippy::too_many_arguments)]
pub fn create_call_tx(
    signer: &PrivateKeySigner,
    to: Address,
    input: alloy::primitives::Bytes,
    value: U256,
    nonce: u64,
    chain_id: u64,
    gas_limit: u64,
    max_priority_fee_per_gas: u128,
) -> Result<Recovered<TxEnvelope>> {
    let tx = TxEip1559 {
        chain_id,
        nonce,
        max_fee_per_gas: 100_000_000_000,
        max_priority_fee_per_gas,
        gas_limit,
        to: TxKind::Call(to),
        value,
        input,
        ..Default::default()
    };

    let signature = signer.sign_hash_sync(&tx.signature_hash())?;
    let signed = TxEnvelope::Eip1559(tx.into_signed(signature));
    let recovered = signed.try_into_recovered()?;
    Ok(recovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accounts_creates_unique_addresses() {
        let accounts = TestAccounts::new();
        let addresses = accounts.addresses();

        assert_eq!(addresses.len(), 3);
        assert_ne!(addresses[0], addresses[1]);
        assert_ne!(addresses[1], addresses[2]);
        assert_ne!(addresses[0], addresses[2]);
    }

    #[test]
    fn test_create_transfer_tx_succeeds() {
        let accounts = TestAccounts::new();
        let tx = create_transfer_tx(
            &accounts.alice,
            accounts.bob_address(),
            U256::from(1_000_000_000_000_000_000u128), // 1 ETH
            0,
            1, // Mainnet chain ID for testing
            10_000,
        );

        assert!(tx.is_ok());
        let recovered = tx.unwrap();
        assert_eq!(recovered.signer(), accounts.alice_address());
    }

    #[test]
    fn test_create_call_tx_succeeds() {
        let accounts = TestAccounts::new();
        let tx = create_call_tx(
            &accounts.alice,
            accounts.bob_address(),
            alloy::primitives::Bytes::from_static(&[0x12, 0x34]),
            U256::ZERO,
            0,
            1,
            50_000,
            10_000,
        );

        assert!(tx.is_ok());
    }
}
