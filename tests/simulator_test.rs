use alloy::consensus::{SignableTransaction as _, TxEip1559, TxEnvelope};
use alloy::eips::BlockId;
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder, RootProvider};
use alloy::rpc::client::RpcClient;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync as _;
use alloy::transports::http::{Client, Http};
use builder::tasks::block::InProgressBlock;
use builder::tasks::simulator::{SimTxEnvelope, SimulatorFactory};
use revm::db::{AlloyDB, CacheDB};
use revm::primitives::{Address, TxKind};
use revm::EvmBuilder;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use trevm::db::sync::{ConcurrentState, ConcurrentStateInfo};
use trevm::revm::primitives::{Account, ExecutionResult, ResultAndState};
use trevm::{BlockDriver, NoopBlock, NoopCfg, TrevmBuilder};

#[tokio::test(flavor = "multi_thread")]
async fn test_spawn() {
    // Create test identity
    let test_wallet = PrivateKeySigner::random();

    // Plumb the transaction pipeline
    let (tx_sender, tx_receiver) = mpsc::unbounded_channel::<Arc<SimTxEnvelope>>();
    let (_bundle_sender, bundle_receiver) = mpsc::unbounded_channel::<Arc<Vec<SimTxEnvelope>>>();
    let deadline = Instant::now() + Duration::from_secs(2);

    // Create a provider
    let root_provider =
        new_rpc_provider("https://sepolia.gateway.tenderly.co".to_string()).unwrap();
    let latest = root_provider.get_block_number().await.unwrap();

    // Create an alloyDB from the provider at the latest height
    let alloy_db = AlloyDB::new(Arc::new(root_provider.clone()), BlockId::from(latest)).unwrap();
    let db = CacheDB::new(Arc::new(alloy_db));

    // Define trevm extension, if any
    let ext = ();

    // Define the evaluator function
    let evaluator = Arc::new(test_evaluator);

    // Create a simulation factory with the provided DB
    let sim_factory = SimulatorFactory::new(db, ext);
    let handle =
        sim_factory.spawn::<SimTxEnvelope, _>(tx_receiver, bundle_receiver, evaluator, deadline);

    // Send some transactions
    for _ in 0..5 {
        let test_tx = Arc::new(SimTxEnvelope(new_test_tx(&test_wallet).unwrap()));
        tx_sender.send(test_tx).unwrap();
    }

    // Wait for the handle to complete
    let best = handle.await.unwrap();

    // Check the result
    assert!(best.is_some());
    let result = best.unwrap();
    assert_ne!(result.score, U256::from(0));

    println!("Best: {:?}", result.score);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_simulator_invalidates_dupe() {
    // Create test identity
    let test_wallet = PrivateKeySigner::random();

    // Create a provider
    let root_provider =
        new_rpc_provider("https://sepolia.gateway.tenderly.co".to_string()).unwrap();
    let latest = root_provider.get_block_number().await.unwrap();

    // Create an alloyDB from the provider at the latest height
    let alloy_db = AlloyDB::new(Arc::new(root_provider.clone()), BlockId::from(latest)).unwrap();
    let db = CacheDB::new(Arc::new(alloy_db));

    // Define trevm extension, if any
    let ext = ();

    // Define the evaluator function
    let evaluator = Arc::new(test_evaluator);

    // Create a simulation factory with the provided DB
    let sim_factory = SimulatorFactory::new(db, ext);

    let test_tx_1 = Arc::new(SimTxEnvelope(new_test_tx(&test_wallet).unwrap()));

    let mut parent_db =
        Arc::new(ConcurrentState::new(sim_factory.db.clone(), ConcurrentStateInfo::default()));

    let child_db = parent_db.child();

    let result = sim_factory.clone().simulate_tx(test_tx_1.clone(), evaluator.clone(), child_db);
    let (best, db) = result.unwrap();
    println!("test success - best returned {}", best.score);

    parent_db.merge_child(db).unwrap();

    let child_2 = parent_db.child();
    let result = sim_factory.simulate_tx(test_tx_1, evaluator, child_2);
    let (best, db) = result.unwrap();

    parent_db.merge_child(db).unwrap();
    println!("test success ?? - best returned {}", best.score);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_block_driver() {
    let anvil =
        alloy::node_bindings::Anvil::new().block_time(1).chain_id(17003).try_spawn().unwrap();
    let keys = anvil.keys();
    
    let anvil_provider = ProviderBuilder::new().on_http(anvil.endpoint_url());
    let latest = anvil_provider.get_block_number().await.unwrap();

    let addresses = anvil.addresses();
    println!("addresses - {:?}", addresses);
    for addr in addresses {
        let balance = anvil_provider.get_balance(*addr).await.unwrap();
        println!("{} - {}", addr, balance);
    }

    // Create an alloyDB from the provider at the latest height
    let alloy_db = AlloyDB::new(Arc::new(anvil_provider.clone()), BlockId::from(latest)).unwrap();
    let db = CacheDB::new(Arc::new(alloy_db));

    let cred = &keys[0];
    let test_wallet = PrivateKeySigner::from_signing_key(cred.into());
    // Create two test transactions that are identical
    let test_tx_1 = Arc::new(SimTxEnvelope(new_test_tx(&test_wallet).unwrap()));

    // Create a new block and ingest the transaction
    let mut block = InProgressBlock::new();
    block.ingest_tx(&test_tx_1.0);

    // Intentionally ingest it a second time to create a dupe
    block.ingest_tx(&test_tx_1.0);
    assert_eq!(block.len(), 2);

    let test_db = ConcurrentState::new(db, ConcurrentStateInfo::default());

    let trevm = EvmBuilder::default()
        .with_db(test_db)
        .build_trevm()
        .fill_cfg(&NoopCfg)
        .fill_block(&NoopBlock);

    let result = block.run_txns(trevm);
    let mut _trevm = result.unwrap();
    let result = _trevm.try_read_account(Address::from_str("0x0000000000000000000000000000000000000000").unwrap()).unwrap();
    match result {
        Some(account) => println!("test_block_driver: account: {:?}", account),
        None => {
            println!("none account found");
        }
    }


    // NB: Bring this up at Friday eng office hours re: James 
    // 
    // NB: Figure out why I am not seeing a Trevm error on the second transaction here.
    // I've tried with and without concurrent state.
    // I've tried nesting up or down in Arcs.
    // I've tried with the child and parent pattern.
    // I've tried it with just raw concurrent states.
    // None of these error or are rejected from what I have seen, and this case _should_ be rejected.
    // The transactions are identical and they both have nonce = 1.

    let addresses = anvil.addresses();
    println!("addresses - {:?}", addresses);
    for addr in addresses {
        let balance = anvil_provider.get_balance(*addr).await.unwrap();
        println!("{} - {}", addr, balance);
    }

    println!("didn't trip on the dupe :( ")
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
    tracing::info!(balance = ?target_account.info.balance, "target account balance");

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
pub fn new_anvil_provider() -> eyre::Result<(RootProvider<Http<Client>>, Vec<Address>)> {
    let anvil =
        alloy::node_bindings::Anvil::new().block_time(1).chain_id(17003).try_spawn().unwrap();
    let addresses = anvil.addresses();
    let root_provider = ProviderBuilder::new().on_http(anvil.endpoint_url());
    Ok((root_provider, addresses.into()))
}
