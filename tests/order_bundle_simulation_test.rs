//! Bundle simulation tests that mirror the Signet Orders docs cases.

use alloy::{
    primitives::{Address, U256},
    serde::OtherFields,
    signers::local::PrivateKeySigner,
};
use builder::test_utils::{
    DEFAULT_BALANCE, DEFAULT_BASEFEE, TestBlockBuildBuilder, TestDbBuilder, TestSimEnvBuilder,
    create_call_tx, create_transfer_tx, scenarios_test_block_env,
};
use signet_bundle::RecoveredBundle;
use signet_constants::SignetSystemConstants;
use signet_sim::SimCache;
use signet_types::{SignedOrder, UnsignedOrder};
use std::time::Duration;

const BLOCK_NUMBER: u64 = 100;
const BLOCK_TIMESTAMP: u64 = 1_700_000_000;

fn generate_funded_accounts(n: usize) -> (Vec<PrivateKeySigner>, TestDbBuilder) {
    let signers: Vec<PrivateKeySigner> = (0..n).map(|_| PrivateKeySigner::random()).collect();
    let balance = U256::from(DEFAULT_BALANCE);

    let mut db_builder = TestDbBuilder::new();
    for signer in &signers {
        db_builder = db_builder.with_account(signer.address(), balance, 0);
    }

    (signers, db_builder)
}

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

fn make_bundle(
    txs: Vec<alloy::consensus::transaction::Recovered<alloy::consensus::TxEnvelope>>,
    host_txs: Vec<alloy::consensus::transaction::Recovered<alloy::consensus::TxEnvelope>>,
    uuid: &str,
) -> RecoveredBundle {
    RecoveredBundle::new_unchecked(
        txs,
        host_txs,
        BLOCK_NUMBER,
        Some(BLOCK_TIMESTAMP - 100),
        Some(BLOCK_TIMESTAMP + 100),
        vec![],
        Some(uuid.to_string()),
        vec![],
        None,
        None,
        vec![],
        OtherFields::default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_onchain_flash_case_cross_chain_flash_borrow_simulates_atomically() {
    let constants = SignetSystemConstants::parmigiana();
    let (signers, db_builder) = generate_funded_accounts(1);
    let signer = &signers[0];

    let recipient = Address::repeat_byte(0xA1);

    let borrow_tx = create_transfer_tx(
        signer,
        recipient,
        U256::from(1_000_000u64),
        0,
        constants.ru_chain_id(),
        12_000_000_000,
    )
    .unwrap();

    let repay_tx = create_transfer_tx(
        signer,
        recipient,
        U256::from(1_001_000u64),
        1,
        constants.ru_chain_id(),
        11_000_000_000,
    )
    .unwrap();

    let cache = SimCache::new();
    cache.add_bundles(
        [make_bundle(vec![borrow_tx, repay_tx], vec![], "onchain-flash")],
        DEFAULT_BASEFEE,
    );

    let built = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(3))
        .build()
        .build()
        .await;

    assert_eq!(built.tx_count(), 2, "flash borrow/repay bundle should land atomically");
    assert_eq!(built.host_transactions().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_onchain_getout_case_exit_with_fee_creates_host_fill() {
    let constants = SignetSystemConstants::parmigiana();
    let (signers, db_builder) = generate_funded_accounts(2);

    let user = &signers[0];
    let filler = &signers[1];

    let recipient = Address::repeat_byte(0xB1);

    // User side lock tx on rollup.
    let ru_lock_tx = create_transfer_tx(
        user,
        recipient,
        U256::from(1_000_000u64),
        0,
        constants.ru_chain_id(),
        10_000_000_000,
    )
    .unwrap();

    // Filler side host payment tx, modeling instant Ethereum delivery.
    let host_fill_tx = create_transfer_tx(
        filler,
        recipient,
        U256::from(995_000u64), // 50 bps fee retained by filler
        0,
        constants.host_chain_id(),
        10_000_000_000,
    )
    .unwrap();

    let cache = SimCache::new();
    cache.add_bundles(
        [make_bundle(vec![ru_lock_tx], vec![host_fill_tx], "onchain-getout")],
        DEFAULT_BASEFEE,
    );

    let built = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(3))
        .build()
        .build()
        .await;

    assert_eq!(built.tx_count(), 1);
    assert_eq!(built.host_transactions().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_onchain_payme_case_output_only_order_host_side_payment() {
    let constants = SignetSystemConstants::parmigiana();
    let (signers, db_builder) = generate_funded_accounts(1);
    let payer = &signers[0];
    let recipient = Address::repeat_byte(0xC1);

    // PayMe docs describe output-only payment requirement.
    // We model this as a contract call gated by a host-side payment tx.
    let ru_gate_call_tx = create_transfer_tx(
        payer,
        recipient,
        U256::from(1u64),
        0,
        constants.ru_chain_id(),
        9_000_000_000,
    )
    .unwrap();

    let host_payment_tx = create_transfer_tx(
        payer,
        recipient,
        U256::from(10_000_000u64),
        0,
        constants.host_chain_id(),
        9_000_000_000,
    )
    .unwrap();

    let cache = SimCache::new();
    cache.add_bundles(
        [make_bundle(vec![ru_gate_call_tx], vec![host_payment_tx], "onchain-payme")],
        DEFAULT_BASEFEE,
    );

    let built = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(3))
        .build()
        .build()
        .await;

    assert_eq!(built.tx_count(), 1);
    assert_eq!(built.host_transactions().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_onchain_payyou_case_input_only_order_bounty() {
    let constants = SignetSystemConstants::parmigiana();
    let (signers, db_builder) = generate_funded_accounts(1);
    let protocol = &signers[0];
    let caller = Address::repeat_byte(0xD1);

    // PayYou docs describe input-only order (bounty) with no required outputs.
    let bounty_tx = create_transfer_tx(
        protocol,
        caller,
        U256::from(10_000_000_000_000_000u64),
        0,
        constants.ru_chain_id(),
        8_000_000_000,
    )
    .unwrap();

    let cache = SimCache::new();
    cache.add_bundles([make_bundle(vec![bounty_tx], vec![], "onchain-payyou")], DEFAULT_BASEFEE);

    let built = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(3))
        .build()
        .build()
        .await;

    assert_eq!(built.tx_count(), 1);
    assert_eq!(built.host_transactions().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_offchain_rust_creating_order_unsigned_order_case() {
    let constants = SignetSystemConstants::parmigiana();
    let recipient = Address::repeat_byte(0xE1);

    let order: UnsignedOrder<'static> = UnsignedOrder::default()
        .with_input(
            signet_constants::parmigiana::RU_WETH,
            U256::from(1_000_000_000_000_000_000u128),
        )
        .with_output(
            signet_constants::parmigiana::HOST_WETH,
            U256::from(1_000_000_000_000_000_000u128),
            recipient,
            constants.host_chain_id() as u32,
        );

    assert_eq!(order.inputs().len(), 1);
    assert_eq!(order.outputs().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_offchain_rust_submitting_order_case_sign_then_bundle_submission() {
    let constants = SignetSystemConstants::parmigiana();
    let (signers, db_builder) = generate_funded_accounts(1);
    let signer = &signers[0];
    let recipient = Address::repeat_byte(0xE2);

    let unsigned: UnsignedOrder<'static> = UnsignedOrder::default()
        .with_input(signet_constants::parmigiana::RU_WETH, U256::from(1_000_000_000_000_000u64))
        .with_output(
            signet_constants::parmigiana::HOST_WETH,
            U256::from(1_000_000_000_000_000u64),
            recipient,
            constants.host_chain_id() as u32,
        )
        .with_chain(&constants)
        .with_nonce(42);

    let signed: SignedOrder = unsigned.sign(signer).await.unwrap();
    assert_eq!(signed.outputs().len(), 1);

    // Model tx-cache forwarding by simulating filler inclusion as a bundle tx.
    let initiate_req = signed.to_initiate_tx(signer.address(), constants.ru_orders());
    let calldata = initiate_req
        .input
        .try_into_unique_input()
        .expect("input and data should agree")
        .unwrap_or_default();

    let initiate_tx = create_call_tx(
        signer,
        constants.ru_orders(),
        calldata,
        U256::ZERO,
        0,
        constants.ru_chain_id(),
        350_000,
        12_000_000_000,
    )
    .unwrap();

    let cache = SimCache::new();
    cache.add_bundles([make_bundle(vec![initiate_tx], vec![], "offchain-submit")], DEFAULT_BASEFEE);

    let built = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(3))
        .build()
        .build()
        .await;

    assert_eq!(built.tx_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_offchain_rust_order_sender_sign_and_send_case() {
    let constants = SignetSystemConstants::parmigiana();
    let (signers, db_builder) = generate_funded_accounts(1);
    let signer = &signers[0];
    let recipient = Address::repeat_byte(0xE3);

    // Emulate OrderSender::sign_and_send_order by signing and immediately adding
    // its initiate call into simulation cache.
    let unsigned: UnsignedOrder<'static> = UnsignedOrder::default()
        .with_input(signet_constants::parmigiana::RU_WETH, U256::from(2_000_000_000_000_000u64))
        .with_output(
            signet_constants::parmigiana::HOST_WETH,
            U256::from(2_000_000_000_000_000u64),
            recipient,
            constants.host_chain_id() as u32,
        )
        .with_chain(&constants)
        .with_nonce(7);

    let signed: SignedOrder = unsigned.sign(signer).await.unwrap();
    let initiate_req = signed.to_initiate_tx(signer.address(), constants.ru_orders());
    let calldata = initiate_req
        .input
        .try_into_unique_input()
        .expect("input and data should agree")
        .unwrap_or_default();

    let initiate_tx = create_call_tx(
        signer,
        constants.ru_orders(),
        calldata,
        U256::ZERO,
        0,
        constants.ru_chain_id(),
        350_000,
        13_000_000_000,
    )
    .unwrap();

    let cache = SimCache::new();
    cache.add_bundles(
        [make_bundle(vec![initiate_tx], vec![], "offchain-sign-and-send")],
        DEFAULT_BASEFEE,
    );

    let built = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(3))
        .build()
        .build()
        .await;

    assert_eq!(built.tx_count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_offchain_rust_order_sender_sign_then_send_separate_case() {
    let constants = SignetSystemConstants::parmigiana();
    let (signers, db_builder) = generate_funded_accounts(1);
    let signer = &signers[0];
    let recipient = Address::repeat_byte(0xE4);

    // Step 1: sign_unsigned_order
    let signed: SignedOrder = UnsignedOrder::default()
        .with_input(signet_constants::parmigiana::RU_WETH, U256::from(3_000_000_000_000_000u64))
        .with_output(
            signet_constants::parmigiana::HOST_WETH,
            U256::from(3_000_000_000_000_000u64),
            recipient,
            constants.host_chain_id() as u32,
        )
        .with_chain(&constants)
        .with_nonce(8)
        .sign(signer)
        .await
        .unwrap();

    // Step 2: send_order (later)
    let initiate_req = signed.to_initiate_tx(signer.address(), constants.ru_orders());
    let calldata = initiate_req
        .input
        .try_into_unique_input()
        .expect("input and data should agree")
        .unwrap_or_default();

    let initiate_tx = create_call_tx(
        signer,
        constants.ru_orders(),
        calldata,
        U256::ZERO,
        0,
        constants.ru_chain_id(),
        350_000,
        14_000_000_000,
    )
    .unwrap();

    let cache = SimCache::new();
    cache.add_bundles(
        [make_bundle(vec![initiate_tx], vec![], "offchain-sign-then-send")],
        DEFAULT_BASEFEE,
    );

    let built = build_env(db_builder)
        .with_cache(cache)
        .with_deadline(Duration::from_secs(3))
        .build()
        .build()
        .await;

    assert_eq!(built.tx_count(), 1);
}
