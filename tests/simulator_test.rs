use alloy::consensus::{SignableTransaction as _, TxEip1559, TxEnvelope};
use alloy::network::Ethereum;
use alloy::primitives::U256;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync as _;
use alloy::transports::http::{Client, Http};
use builder::config::WalletlessProvider;
use builder::tasks::simulator::SimulatorFactory;
use trevm::revm::database::{AlloyDB, CacheDB};
use trevm::revm::primitives::{address, TxKind};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use trevm::revm::primitives::{Account, ExecutionResult, ResultAndState};
use alloy::eips::BlockId;
use alloy::providers::{Provider, ProviderBuilder};

#[tokio::test(flavor = "multi_thread")]
async fn test_spawn() {
    // Setup transaction pipeline plumbing
    let (tx_sender, tx_receiver) = mpsc::unbounded_channel::<TxEnvelope>();
    let (_bundle_sender, bundle_receiver) = mpsc::unbounded_channel::<Vec<TxEnvelope>>();
    let deadline = Instant::now() + Duration::from_secs(2);

    // Create a new anvil instance
    let anvil =
        alloy::node_bindings::Anvil::new().block_time(1).chain_id(17003).try_spawn().unwrap();

    // Create a test wallet from the anvil keys
    let keys = anvil.keys();
    let test_wallet = &PrivateKeySigner::from(keys[0].clone());

    // Create a root provider on that anvil instance
    let root_provider = ProviderBuilder::new().on_http(anvil.endpoint_url());
    let latest = root_provider.get_block_number().await.unwrap();

    // Create an alloyDB from the provider at the latest height
    let alloy_db: AlloyDB<Http<Client>, Ethereum, Arc<WalletlessProvider>> =
        AlloyDB::new(Arc::new(root_provider.clone()), BlockId::Number(latest.into())).unwrap();
    let db = CacheDB::new(Arc::new(alloy_db));

    // Define trevm extension, if any
    let ext = ();

    // Define the evaluator function
    let evaluator = Arc::new(test_evaluator);

    // Create a simulation factory with the provided DB
    let sim_factory = SimulatorFactory::new(db, ext);
    let handle =
        sim_factory.spawn::<TxEnvelope, _>(tx_receiver, evaluator, deadline);

    // Send some transactions
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
