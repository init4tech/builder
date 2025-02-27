use alloy::consensus::{SignableTransaction as _, TxEip1559, TxEnvelope};
use alloy::eips::BlockId;
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::RpcClient;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync as _;
use builder::tasks::simulator::{SimBundle, SimTxEnvelope, SimulatorFactory};
use revm::db::{AlloyDB, CacheDB};
use revm::primitives::{Address, TxKind};
use revm::Database;
use trevm::Tx;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use trevm::revm::primitives::{Account, ExecutionResult, ResultAndState};

#[tokio::test(flavor = "multi_thread")]
async fn test_spawn() {
    // Create test identity
    let test_wallet = PrivateKeySigner::random();

    // Plumb the transaction pipeline
    let (tx_sender, tx_receiver) = mpsc::unbounded_channel::<Arc<SimTxEnvelope>>();
    let (bundle_sender, bundle_receiver) = mpsc::unbounded_channel::<Arc<SimBundle>>();
    let deadline = Instant::now() + Duration::from_secs(2);

    // Create an RPC provider from the rollup
    let url = "https://rpc.havarti.signet.sh ".parse().unwrap();
    let root_provider = ProviderBuilder::new().on_client(RpcClient::new_http(url));

    let block_number = root_provider.get_block_number().await.unwrap();
    assert_ne!(block_number, 0, "root provider is reporting block number 0");

    let latest = root_provider.get_block_number().await.unwrap();
    assert!(latest > 0);

    let db = AlloyDB::new(Arc::new(root_provider.clone()), BlockId::from(latest)).unwrap();
    let alloy_db = Arc::new(db);

    let ext = ();

    let evaluator = Arc::new(|_state: &ResultAndState| U256::from(1));

    let sim_factory = SimulatorFactory::new(CacheDB::new(alloy_db), ext);
    let handle = sim_factory.spawn::<SimTxEnvelope, _>(tx_receiver, bundle_receiver, evaluator, deadline);

    // Send some transactions
    for _ in 0..5 {
        let test_tx = Arc::new(SimTxEnvelope(new_test_tx(&test_wallet).unwrap()));
        println!("dispatching tx {:?}", test_tx.0);
        tx_sender.send(test_tx).unwrap();
    }

    // Wait for the handle to complete
    let best = handle.await.unwrap();

    // Check the result
    assert!(best.is_some());
    assert_eq!(best.unwrap().score, U256::from(0));
}

fn mock_evaluator(state: &ResultAndState) -> U256 {
    // log the transaction results
    match &state.result {
        ExecutionResult::Success { .. } => println!("Execution was successful."),
        ExecutionResult::Revert { .. } => println!("Execution reverted."),
        ExecutionResult::Halt { .. } => println!("Execution halted."),
    }

    // return the target account balance
    let target_addr = Address::from_str("0x0000000000000000000000000000000000000000").unwrap();
    let default_account = Account::default();
    let target_account = state.state.get(&target_addr).unwrap_or(&default_account);
    println!("target account balance: {:?}", target_account.info.balance);
    target_account.info.balance
}

// Returns a new signed test transaction with default values
fn new_test_tx(wallet: &PrivateKeySigner) -> eyre::Result<TxEnvelope> {
    let tx = TxEip1559 {
        chain_id: 17001,
        nonce: 1,
        gas_limit: 50000,
        to: TxKind::Call(Address::from_str("0x0000000000000000000000000000000000000000").unwrap()),
        value: U256::from(1_f64),
        input: alloy::primitives::bytes!(""),
        ..Default::default()
    };
    let signature = wallet.sign_hash_sync(&tx.signature_hash())?;
    Ok(TxEnvelope::Eip1559(tx.into_signed(signature)))
}
