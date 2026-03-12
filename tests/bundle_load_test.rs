//! Load tests for bundle simulation.
//!
//! These tests exercise the block building loop with high volumes of bundles
//! and transactions to verify correctness and deadline compliance under stress.

use alloy::{
    primitives::{Address, U256},
    serde::OtherFields,
    signers::local::PrivateKeySigner,
};
use builder::test_utils::{
    DEFAULT_BALANCE, DEFAULT_BASEFEE, TestBlockBuildBuilder, TestDbBuilder, TestSimEnvBuilder,
    create_transfer_tx, scenarios_test_block_env,
};
use signet_bundle::RecoveredBundle;
use signet_sim::{BuiltBlock, SimCache};
use std::collections::HashSet;
use std::time::Duration;

/// Block number used for all test environments and bundles.
const BLOCK_NUMBER: u64 = 100;

/// Block timestamp used for all test environments and bundles.
const BLOCK_TIMESTAMP: u64 = 1_700_000_000;

/// Parmigiana rollup chain ID.
const RU_CHAIN_ID: u64 = 88888;

/// Default max priority fee used for transfer bundles in tests.
const DEFAULT_PRIORITY_FEE: u128 = 10_000_000_000;

/// Generate N random funded signers and a database builder with all of them funded.
fn generate_funded_accounts(n: usize) -> (Vec<PrivateKeySigner>, TestDbBuilder) {
    let signers: Vec<PrivateKeySigner> = (0..n).map(|_| PrivateKeySigner::random()).collect();
    let balance = U256::from(DEFAULT_BALANCE);

    let mut db_builder = TestDbBuilder::new();
    for signer in &signers {
        db_builder = db_builder.with_account(signer.address(), balance, 0);
    }

    (signers, db_builder)
}

/// Create a `RecoveredBundle` with one transfer transaction.
fn make_bundle(
    signer: &PrivateKeySigner,
    to: Address,
    uuid: String,
    max_priority_fee: u128,
) -> RecoveredBundle {
    let tx = create_transfer_tx(signer, to, U256::from(1_000u64), 0, RU_CHAIN_ID, max_priority_fee)
        .unwrap();

    RecoveredBundle::new_unchecked(
        vec![tx],
        vec![],
        BLOCK_NUMBER,
        Some(BLOCK_TIMESTAMP - 100),
        Some(BLOCK_TIMESTAMP + 100),
        vec![],
        Some(uuid),
        vec![],
        None,
        None,
        vec![],
        OtherFields::default(),
    )
}

/// Build a `TestBlockBuildBuilder` from a pre-funded db builder.
fn build_env(db_builder: TestDbBuilder) -> TestBlockBuildBuilder {
    let db = db_builder.build();
    let block_env =
        scenarios_test_block_env(BLOCK_NUMBER, DEFAULT_BASEFEE, BLOCK_TIMESTAMP, 3_000_000_000);
    let sim_env = TestSimEnvBuilder::new()
        .with_rollup_db(db.clone())
        .with_host_db(db)
        .with_block_env(block_env);
    TestBlockBuildBuilder::new().with_sim_env_builder(sim_env)
}

/// 50 bundles each containing 1 transfer tx. Verify block builds and includes txs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_load_many_bundles() {
    let count = 50;
    let (signers, db_builder) = generate_funded_accounts(count);
    let recipient = Address::repeat_byte(0xAA);

    let cache = SimCache::with_capacity(count);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(i, signer)| {
            make_bundle(signer, recipient, format!("bundle-{i}"), DEFAULT_PRIORITY_FEE)
        })
        .collect();

    cache.add_bundles(bundles, DEFAULT_BASEFEE);
    assert_eq!(cache.len(), count);

    let builder = build_env(db_builder).with_cache(cache).with_deadline(Duration::from_secs(5));
    let built: BuiltBlock = builder.build().build().await;

    assert!(built.tx_count() > 0, "expected transactions in built block, got 0");
    assert_eq!(
        built.tx_count(),
        count,
        "expected all {count} bundle txs to be included, got {}",
        built.tx_count()
    );
}

/// 50k bundles each containing 1 transfer tx. Verify block builds with non-zero count of included txs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_load_50k_bundles() {
    let count = 50_000;
    let (signers, db_builder) = generate_funded_accounts(count);
    let recipient = Address::repeat_byte(0xAA);

    let cache = SimCache::with_capacity(count);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(i, signer)| {
            // Keep ranks distinct to avoid pathological cache insertion cost at high volume.
            make_bundle(signer, recipient, format!("bundle-{i}"), DEFAULT_PRIORITY_FEE + i as u128)
        })
        .collect();

    cache.add_bundles(bundles, DEFAULT_BASEFEE);
    assert_eq!(cache.len(), count);

    let builder = build_env(db_builder).with_cache(cache).with_deadline(Duration::from_secs(12));
    let built: BuiltBlock = builder.build().build().await;

    assert!(built.tx_count() > 0, "expected transactions in built block, got 0");
}

/// 30 bundles + 30 standalone txs. Verify both types land in the built block.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_load_bundles_and_txs_mixed() {
    let bundle_count = 30;
    let tx_count = 30;
    let total = bundle_count + tx_count;

    let (signers, db_builder) = generate_funded_accounts(total);
    let recipient = Address::repeat_byte(0xBB);

    let cache = SimCache::with_capacity(total);

    let bundles: Vec<RecoveredBundle> = signers[..bundle_count]
        .iter()
        .enumerate()
        .map(|(i, signer)| {
            make_bundle(signer, recipient, format!("mix-bundle-{i}"), DEFAULT_PRIORITY_FEE)
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    for signer in &signers[bundle_count..] {
        let tx = create_transfer_tx(
            signer,
            recipient,
            U256::from(1_000u64),
            0,
            RU_CHAIN_ID,
            10_000_000_000,
        )
        .unwrap();
        cache.add_tx(tx, DEFAULT_BASEFEE);
    }

    assert_eq!(cache.len(), total);

    let builder = build_env(db_builder).with_cache(cache).with_deadline(Duration::from_secs(5));
    let built: BuiltBlock = builder.build().build().await;

    assert!(built.tx_count() > 0, "expected transactions in built block");
    assert_eq!(
        built.tx_count(),
        total,
        "expected all {total} items included, got {}",
        built.tx_count()
    );
}

/// Many bundles with a constrained gas limit. Verify gas cap is respected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_load_saturate_gas_limit() {
    let count = 50;
    let (signers, db_builder) = generate_funded_accounts(count);
    let recipient = Address::repeat_byte(0xCC);

    let cache = SimCache::with_capacity(count);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(i, signer)| {
            make_bundle(signer, recipient, format!("gas-bundle-{i}"), DEFAULT_PRIORITY_FEE)
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    // Each transfer costs 21,000 gas. Allow room for ~10 transfers.
    let max_gas: u64 = 21_000 * 10;

    let builder = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(5))
        .with_max_gas(max_gas);
    let built: BuiltBlock = builder.build().build().await;

    assert!(
        built.tx_count() <= 10,
        "expected at most 10 txs within gas limit, got {}",
        built.tx_count()
    );
    assert!(built.tx_count() > 0, "expected at least some txs to be included");
}

/// Many bundles with a tight deadline. Verify block completes within time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_load_deadline_pressure() {
    let count = 100;
    let (signers, db_builder) = generate_funded_accounts(count);
    let recipient = Address::repeat_byte(0xDD);

    let cache = SimCache::with_capacity(count);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(i, signer)| {
            make_bundle(signer, recipient, format!("deadline-bundle-{i}"), DEFAULT_PRIORITY_FEE)
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    let deadline = Duration::from_millis(500);
    let start = std::time::Instant::now();

    let builder = build_env(db_builder).with_cache(cache).with_deadline(deadline);
    let built: BuiltBlock = builder.build().build().await;

    let elapsed = start.elapsed();

    assert!(built.tx_count() > 0, "expected at least some txs under deadline pressure");

    // Should complete within a reasonable margin of the deadline.
    assert!(elapsed < deadline * 3, "block build took {elapsed:?}, expected within ~{deadline:?}");
}

/// Gas-constrained block: verify the builder selects the highest-fee bundles first.
///
/// 10 low-fee bundles + 10 high-fee bundles, with a gas cap that can only fit 10.
/// Every included transaction must originate from a high-fee sender.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_load_fee_priority_ordering() {
    let low_count = 10usize;
    let high_count = 10usize;
    let total = low_count + high_count;

    let (signers, db_builder) = generate_funded_accounts(total);
    let recipient = Address::repeat_byte(0xEE);

    let cache = SimCache::with_capacity(total);

    let low_fee = DEFAULT_PRIORITY_FEE;
    let high_fee = 90_000_000_000u128; // 90 Gwei — valid (<= max_fee_per_gas=100 Gwei), 9× above low_fee

    let low_fee_senders: HashSet<Address> =
        signers[..low_count].iter().map(|s| s.address()).collect();

    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(i, signer)| {
            let fee = if i < low_count { low_fee } else { high_fee };
            make_bundle(signer, recipient, format!("priority-bundle-{i}"), fee)
        })
        .collect();

    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    // Gas limit exactly fits the 10 high-fee bundles (21,000 gas each).
    let max_gas: u64 = 21_000 * high_count as u64;

    let builder = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(5))
        .with_max_gas(max_gas);
    let built: BuiltBlock = builder.build().build().await;

    assert_eq!(
        built.tx_count(),
        high_count,
        "expected exactly {high_count} txs, got {}",
        built.tx_count()
    );

    for tx in built.transactions() {
        assert!(
            !low_fee_senders.contains(&tx.signer()),
            "low-fee sender {} was included instead of a high-fee sender",
            tx.signer()
        );
    }
}
