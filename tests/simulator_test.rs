use alloy::{
    consensus::{SignableTransaction as _, TxEip1559, TxEnvelope},
    primitives::U256,
    signers::{
        SignerSync as _,
        local::{LocalSigner, PrivateKeySigner},
    },
};
use builder::tasks::simulator::SimulatorFactory;
use std::sync::Arc;
use tokio::{
    sync::mpsc,
    time::{Duration, Instant},
};
use trevm::revm::{
    context::result::{ExecutionResult, ResultAndState},
    database::{CacheDB, InMemoryDB},
    inspector::NoOpInspector,
    primitives::{TxKind, address},
    state::{Account, AccountInfo},
};

#[tokio::test(flavor = "multi_thread")]
async fn test_spawn() {
    // Setup transaction pipeline plumbing
    let unbounded_channel = mpsc::unbounded_channel::<TxEnvelope>();
    let (tx_sender, tx_receiver) = unbounded_channel;
    let (_bundle_sender, _bundle_receiver) = mpsc::unbounded_channel::<Vec<TxEnvelope>>();
    let deadline = Instant::now() + Duration::from_secs(5);

    // Create a new anvil instance and test wallets
    let anvil =
        alloy::node_bindings::Anvil::new().block_time(1).chain_id(14174).try_spawn().unwrap();
    let keys = anvil.keys();
    let test_wallet = &PrivateKeySigner::from(keys[0].clone());

    // Create a evaluator
    let evaluator = Arc::new(test_evaluator);

    // Make a database and seed it with some starting account state
    let db = seed_database(CacheDB::new(InMemoryDB::default()), test_wallet);

    // Create a new simulator factory with the given database and inspector
    let sim_factory = SimulatorFactory::new(db, NoOpInspector);

    // Spawn the simulator actor
    let handle = sim_factory.spawn(tx_receiver, evaluator, deadline);

    // Send transactions to the simulator
    for count in 0..2 {
        let test_tx = new_test_tx(test_wallet, count).unwrap();
        tx_sender.send(test_tx).unwrap();
    }

    // Wait for simulation to complete
    let best = handle.await.unwrap();

    // Assert on the block
    assert_eq!(best.len(), 1);
}

/// An example of a simple evaluator function for use in testing
fn test_evaluator(state: &ResultAndState) -> U256 {
    // log the transaction results
    match &state.result {
        ExecutionResult::Success { .. } => println!("Execution was successful."),
        ExecutionResult::Revert { .. } => println!("Execution reverted."),
        ExecutionResult::Halt { .. } => println!("Execution halted."),
    }

    // return the target account balance
    let target_addr = address!("0x0000000000000000000000000000000000000000");
    let default_account = Account::default();
    let target_account = state.state.get(&target_addr).unwrap_or(&default_account);
    tracing::info!(balance = ?target_account.info.balance, "target account balance");

    target_account.info.balance
}

// Returns a new signed test transaction with default values
fn new_test_tx(wallet: &PrivateKeySigner, nonce: u64) -> eyre::Result<TxEnvelope> {
    let tx = TxEip1559 {
        chain_id: 17003,
        gas_limit: 50000,
        nonce,
        to: TxKind::Call(address!("0x0000000000000000000000000000000000000000")),
        value: U256::from(1),
        input: alloy::primitives::bytes!(""),
        ..Default::default()
    };
    let signature = wallet.sign_hash_sync(&tx.signature_hash())?;
    Ok(TxEnvelope::Eip1559(tx.into_signed(signature)))
}

// Adds a balance to the given wallet address in the database for simple simulation unit tests
fn seed_database(mut db: CacheDB<InMemoryDB>, wallet: &PrivateKeySigner) -> CacheDB<InMemoryDB> {
    let mut info = AccountInfo::default();
    info.balance = U256::from(10000);
    db.insert_account_info(wallet.address(), info);

    db
}
