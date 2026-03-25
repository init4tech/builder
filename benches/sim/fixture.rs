//! Shared fixtures and setup functions for simulation benchmarks.

use alloy::{
    primitives::{Address, Bytes, U256},
    serde::OtherFields,
    signers::local::PrivateKeySigner,
};
use builder::test_utils::{
    DEFAULT_BALANCE, DEFAULT_BASEFEE, TestDb, TestDbBuilder, TestHostEnv, TestRollupEnv,
    TestSimEnvBuilder, TestStateSource, create_call_tx, create_transfer_tx,
    scenarios_test_block_env,
};
use signet_bundle::RecoveredBundle;
use signet_sim::{SharedSimEnv, SimCache};
use std::time::Duration;
use tokio::time::Instant;
use trevm::revm::bytecode::Bytecode;
use trevm::revm::inspector::NoOpInspector;

pub const BLOCK_NUMBER: u64 = 100;
pub const BLOCK_TIMESTAMP: u64 = 1_700_000_000;
pub const RU_CHAIN_ID: u64 = 88888;
pub const DEFAULT_PRIORITY_FEE: u128 = 10_000_000_000;
pub const MAX_GAS: u64 = 3_000_000_000;
pub const MAX_HOST_GAS: u64 = 24_000_000;
pub const REVERTING_CONTRACT: Address = Address::repeat_byte(0xBB);
/// Latency values based on real-world RPC provider benchmarks:
/// - 0ms:   baseline, no network simulation
/// - 50ms:  good provider, same-region (p50 for top providers)
/// - 200ms: cross-region or average provider
pub const LATENCIES: [(&str, Duration); 3] = [
    ("0ms", Duration::ZERO),
    ("50ms", Duration::from_millis(50)),
    ("200ms", Duration::from_millis(200)),
];

/// Type alias for the shared simulation environment used in benchmarks.
pub type BenchSimEnv = SharedSimEnv<TestDb, TestDb, NoOpInspector, NoOpInspector>;

/// Everything needed to run a benchmark iteration: the simulation environment
/// plus state sources for preflight validity checks.
pub struct Fixture {
    env: BenchSimEnv,
    rollup_source: TestStateSource,
    host_source: TestStateSource,
}

impl Fixture {
    fn new(envs: Envs, concurrency: usize, cache: SimCache) -> Self {
        let Envs { rollup_env, host_env, rollup_source, host_source } = envs;
        // Set a deadline far in the future so that `sim_round()` never short-circuits.
        let finish_by = Instant::now() + Duration::from_secs(3600);
        let env = SharedSimEnv::new(rollup_env, host_env, finish_by, concurrency, cache);
        Fixture { env, rollup_source, host_source }
    }

    /// Drain all items from the sim env via repeated `sim_round()` calls.
    pub async fn run(self) {
        let Fixture { mut env, rollup_source, host_source } = self;
        while env.sim_round(MAX_GAS, MAX_HOST_GAS, &rollup_source, &host_source).await.is_some() {}
    }
}

struct Envs {
    rollup_env: TestRollupEnv,
    host_env: TestHostEnv,
    rollup_source: TestStateSource,
    host_source: TestStateSource,
}

impl Envs {
    /// Build simulation environments and state sources from a shared database.
    fn new(db: TestDb) -> Self {
        let block_env =
            scenarios_test_block_env(BLOCK_NUMBER, DEFAULT_BASEFEE, BLOCK_TIMESTAMP, MAX_GAS);

        let sim_env = TestSimEnvBuilder::new()
            .with_rollup_db(db.clone())
            .with_host_db(db.clone())
            .with_block_env(block_env);

        let rollup_source = TestStateSource::new(db.clone());
        let host_source = TestStateSource::new(db);
        let (rollup_env, host_env) = sim_env.build();
        Envs { rollup_env, host_env, rollup_source, host_source }
    }
}

/// Generate `count` random funded signers.
fn generate_signers(count: usize) -> Vec<PrivateKeySigner> {
    (0..count).map(|_| PrivateKeySigner::random()).collect()
}

/// Create a `TestDbBuilder` with accounts funded from the given signers.
fn fund_accounts(signers: &[PrivateKeySigner]) -> TestDbBuilder {
    let balance = U256::from(DEFAULT_BALANCE);
    let mut builder = TestDbBuilder::new();
    for signer in signers {
        builder = builder.with_account(signer.address(), balance, 0);
    }
    builder
}

/// Create a `RecoveredBundle` with one transfer transaction.
fn make_bundle(
    signer: &PrivateKeySigner,
    to: Address,
    nonce: u64,
    uuid: String,
    max_priority_fee: u128,
) -> RecoveredBundle {
    let tx =
        create_transfer_tx(signer, to, U256::from(1_000u64), nonce, RU_CHAIN_ID, max_priority_fee)
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

/// Build a [`SharedSimEnv`] and state sources with bundles in the cache.
///
/// All databases use the same latency to model production RPC round-trips
/// (both simulation environments and preflight state sources hit the RPC).
pub fn set_up_bundle_sim(count: usize, latency: Duration, concurrency: usize) -> Fixture {
    let signers = generate_signers(count);
    let db = fund_accounts(&signers).with_latency(latency).build();
    let envs = Envs::new(db);

    let cache = SimCache::with_capacity(count);
    let recipient = Address::repeat_byte(0xAA);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(idx, signer)| {
            make_bundle(signer, recipient, 0, format!("bench-{idx}"), DEFAULT_PRIORITY_FEE)
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    Fixture::new(envs, concurrency, cache)
}

/// Build a [`SharedSimEnv`] and state sources with standalone txs in the cache.
pub fn set_up_tx_sim(count: usize, latency: Duration, concurrency: usize) -> Fixture {
    let signers = generate_signers(count);
    let db = fund_accounts(&signers).with_latency(latency).build();
    let envs = Envs::new(db);

    let cache = SimCache::with_capacity(count);
    let recipient = Address::repeat_byte(0xAA);
    for signer in &signers {
        let tx = create_transfer_tx(
            signer,
            recipient,
            U256::from(1_000u64),
            0,
            RU_CHAIN_ID,
            DEFAULT_PRIORITY_FEE,
        )
        .unwrap();
        cache.add_tx(tx, DEFAULT_BASEFEE);
    }

    Fixture::new(envs, concurrency, cache)
}

/// Build a [`SharedSimEnv`] with bundles that all fail preflight validation.
///
/// Accounts are funded with nonce 1, but all bundles use nonce 0. Since
/// `state_nonce > tx_nonce`, every item is marked `Never` and removed from the
/// cache on the first `sim_round()` call.
pub fn set_up_preflight_failure_sim(
    count: usize,
    latency: Duration,
    concurrency: usize,
) -> Fixture {
    let signers = generate_signers(count);

    let balance = U256::from(DEFAULT_BALANCE);
    let mut db_builder = TestDbBuilder::new();
    for signer in &signers {
        db_builder = db_builder.with_account(signer.address(), balance, 1);
    }
    let db = db_builder.with_latency(latency).build();
    let envs = Envs::new(db);

    let cache = SimCache::with_capacity(count);
    let recipient = Address::repeat_byte(0xAA);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(idx, signer)| {
            make_bundle(signer, recipient, 0, format!("bench-{idx}"), DEFAULT_PRIORITY_FEE)
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    Fixture::new(envs, concurrency, cache)
}

/// Build a [`SharedSimEnv`] with bundles that pass preflight but revert during
/// EVM execution.
///
/// A contract at [`REVERTING_CONTRACT`] contains `PUSH0 PUSH0 REVERT` (always
/// reverts with empty data). All bundles call this contract, so they pass
/// preflight (valid nonce + sufficient balance) but fail during simulation.
pub fn set_up_execution_failure_sim(
    count: usize,
    latency: Duration,
    concurrency: usize,
) -> Fixture {
    let signers = generate_signers(count);

    // PUSH0 PUSH0 REVERT — always reverts with empty returndata.
    let revert_bytecode = Bytecode::new_raw(Bytes::from_static(&[0x5F, 0x5F, 0xFD]));

    let balance = U256::from(DEFAULT_BALANCE);
    let mut db_builder = TestDbBuilder::new().with_contract(REVERTING_CONTRACT, revert_bytecode);
    for signer in &signers {
        db_builder = db_builder.with_account(signer.address(), balance, 0);
    }
    let db = db_builder.with_latency(latency).build();
    let envs = Envs::new(db);

    let cache = SimCache::with_capacity(count);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(idx, signer)| {
            let tx = create_call_tx(
                signer,
                REVERTING_CONTRACT,
                Bytes::new(),
                U256::ZERO,
                0,
                RU_CHAIN_ID,
                50_000,
                DEFAULT_PRIORITY_FEE,
            )
            .unwrap();
            RecoveredBundle::new_unchecked(
                vec![tx],
                vec![],
                BLOCK_NUMBER,
                Some(BLOCK_TIMESTAMP - 100),
                Some(BLOCK_TIMESTAMP + 100),
                vec![],
                Some(format!("bench-{idx}")),
                vec![],
                None,
                None,
                vec![],
                OtherFields::default(),
            )
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    Fixture::new(envs, concurrency, cache)
}

/// Build a [`SharedSimEnv`] with `bundle_count` bundles spread across `num_senders` signers.
///
/// Each signer sends `bundle_count / num_senders` bundles with incrementing nonces.
pub fn set_up_sender_distribution_sim(
    bundle_count: usize,
    num_senders: usize,
    concurrency: usize,
) -> Fixture {
    let signers = generate_signers(num_senders);
    let bundles_per_sender = bundle_count / num_senders;

    let balance = U256::from(DEFAULT_BALANCE);
    let mut db_builder = TestDbBuilder::new();
    for signer in &signers {
        db_builder = db_builder.with_account(signer.address(), balance, 0);
    }
    let db = db_builder.build();
    let envs = Envs::new(db);

    let cache = SimCache::with_capacity(bundle_count);
    let recipient = Address::repeat_byte(0xAA);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .flat_map(|(sender_idx, signer)| {
            (0..bundles_per_sender).map(move |nonce| {
                make_bundle(
                    signer,
                    recipient,
                    nonce as u64,
                    format!("bench-{sender_idx}-{nonce}"),
                    DEFAULT_PRIORITY_FEE,
                )
            })
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    Fixture::new(envs, concurrency, cache)
}

/// Build a [`SharedSimEnv`] with bundles containing multiple rollup transactions each.
///
/// Each bundle has `txs_per_bundle` transfer transactions from the same sender with
/// incrementing nonces. This measures the cost of simulating larger bundles where
/// all transactions must succeed atomically.
pub fn set_up_multi_tx_bundle_sim(
    bundle_count: usize,
    txs_per_bundle: usize,
    concurrency: usize,
) -> Fixture {
    let signers = generate_signers(bundle_count);
    let db = fund_accounts(&signers).build();
    let envs = Envs::new(db);

    let cache = SimCache::with_capacity(bundle_count);
    let recipient = Address::repeat_byte(0xAA);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(idx, signer)| {
            let txs: Vec<_> = (0..txs_per_bundle)
                .map(|nonce| {
                    create_transfer_tx(
                        signer,
                        recipient,
                        U256::from(1_000u64),
                        nonce as u64,
                        RU_CHAIN_ID,
                        DEFAULT_PRIORITY_FEE,
                    )
                    .unwrap()
                })
                .collect();
            RecoveredBundle::new_unchecked(
                txs,
                vec![],
                BLOCK_NUMBER,
                Some(BLOCK_TIMESTAMP - 100),
                Some(BLOCK_TIMESTAMP + 100),
                vec![],
                Some(format!("bench-{idx}")),
                vec![],
                None,
                None,
                vec![],
                OtherFields::default(),
            )
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    Fixture::new(envs, concurrency, cache)
}

/// Build a [`SharedSimEnv`] with bundles that include host-chain transactions.
///
/// Each bundle has one rollup transfer plus `host_txs_per_bundle` host-chain transfers.
/// This measures the overhead of bundles that carry cross-chain transactions.
pub fn set_up_host_tx_bundle_sim(
    bundle_count: usize,
    host_txs_per_bundle: usize,
    concurrency: usize,
) -> Fixture {
    let signers = generate_signers(bundle_count);
    let db = fund_accounts(&signers).build();
    let envs = Envs::new(db);

    let cache = SimCache::with_capacity(bundle_count);
    let recipient = Address::repeat_byte(0xAA);
    let bundles: Vec<RecoveredBundle> = signers
        .iter()
        .enumerate()
        .map(|(idx, signer)| {
            let rollup_tx = create_transfer_tx(
                signer,
                recipient,
                U256::from(1_000u64),
                0,
                RU_CHAIN_ID,
                DEFAULT_PRIORITY_FEE,
            )
            .unwrap();
            let host_txs: Vec<_> = (0..host_txs_per_bundle)
                .map(|nonce| {
                    create_transfer_tx(
                        signer,
                        recipient,
                        U256::from(1_000u64),
                        (nonce + 1) as u64,
                        RU_CHAIN_ID,
                        DEFAULT_PRIORITY_FEE,
                    )
                    .unwrap()
                })
                .collect();
            RecoveredBundle::new_unchecked(
                vec![rollup_tx],
                host_txs,
                BLOCK_NUMBER,
                Some(BLOCK_TIMESTAMP - 100),
                Some(BLOCK_TIMESTAMP + 100),
                vec![],
                Some(format!("bench-{idx}")),
                vec![],
                None,
                None,
                vec![],
                OtherFields::default(),
            )
        })
        .collect();
    cache.add_bundles(bundles, DEFAULT_BASEFEE);

    Fixture::new(envs, concurrency, cache)
}
