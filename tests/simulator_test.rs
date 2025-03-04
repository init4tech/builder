use alloy::consensus::{SignableTransaction as _, TxEip1559, TxEnvelope};
use alloy::eips::BlockId;
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::client::RpcClient;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync as _;
use alloy::transports::http::{Client, Http};
use builder::tasks::simulator::{SimBundle, SimTxEnvelope, SimulatorFactory};
use revm::db::{AlloyDB, CacheDB};
use revm::primitives::{Address, TxKind};
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
    let (_bundle_sender, bundle_receiver) = mpsc::unbounded_channel::<Arc<SimBundle>>();
    let deadline = Instant::now() + Duration::from_secs(2);

    // Create a provider
    let root_provider =
        new_rpc_provider("https://sepolia.gateway.tenderly.co".to_string()).unwrap();
    let latest = root_provider.get_block_number().await.unwrap();

    let db = AlloyDB::new(Arc::new(root_provider.clone()), BlockId::from(latest)).unwrap();
    let alloy_db = Arc::new(db);

    let ext = ();

    // Define the evaluator function
    let evaluator = Arc::new(test_evaluator);

    // Create a simulation factory
    let sim_factory = SimulatorFactory::new(CacheDB::new(alloy_db), ext);
    let handle =
        sim_factory.spawn::<SimTxEnvelope, _>(tx_receiver, bundle_receiver, evaluator, deadline);

    // Send some transactions
    for _ in 0..5 {
        let test_tx = Arc::new(SimTxEnvelope(new_test_tx(&test_wallet).unwrap()));
        // println!("dispatching tx {:?}", test_tx.0);
        tx_sender.send(test_tx).unwrap();
    }

    // Wait for the handle to complete
    let best = handle.await.unwrap();

    // Check the result
    assert!(best.is_some());
    assert_ne!(best.unwrap().score, U256::from(0));
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

/// Returns a new RPC provider from a given URL
pub fn new_rpc_provider(url: String) -> eyre::Result<RootProvider<Http<Client>>> {
    let url = url.parse().unwrap();
    let root_provider = ProviderBuilder::new().on_client(RpcClient::new_http(url));
    Ok(root_provider)
}

/// Returns a provider based on a local Anvil instance that it creates
pub fn new_anvil_provider() -> eyre::Result<RootProvider<Http<Client>>> {
    let anvil =
        alloy::node_bindings::Anvil::new().block_time(1).chain_id(17003).try_spawn().unwrap();
    let root_provider = ProviderBuilder::new().on_http(anvil.endpoint_url());
    Ok(root_provider)
}
