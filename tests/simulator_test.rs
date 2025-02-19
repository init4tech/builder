use alloy::consensus::{SignableTransaction as _, TxEip1559, TxEnvelope};
use alloy::eips::BlockId;
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::client::RpcClient;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync as _;
use builder::tasks::simulator::{EvmPool, SimTxEnvelope, SimulatorFactory};
use revm::db::{AlloyDB, CacheDB};
use revm::primitives::{Address, TxKind};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use trevm::revm::primitives::ResultAndState;
use trevm::{NoopBlock, NoopCfg};

#[tokio::test]
async fn test_spawn() {
    // Create test identity
    let test_wallet = PrivateKeySigner::random();

    // Plumb the transaction pipeline
    let (tx_sender, tx_receiver) = mpsc::unbounded_channel::<Arc<SimTxEnvelope>>();
    let deadline = Instant::now() + Duration::from_secs(5);

    // Create an RPC provider for a data source
    let url = "https://eth.merkle.io".parse().unwrap();
    let rpc_client = RpcClient::new_http(url);
    let root_provider = ProviderBuilder::new().on_client(rpc_client.clone());

    // TODO: Add a sanity check that the root_provider has real access and block numbers

    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let latest = root_provider.get_block_number().await.unwrap();
    let alloy_db = Arc::new(AlloyDB::with_runtime(root_provider.clone(), BlockId::from(latest), runtime));
    let ext = ();

    let evm_factory = SimulatorFactory::new(CacheDB::new(alloy_db), ext);
    let evm_pool = EvmPool::new(evm_factory, NoopCfg, NoopBlock);

    // Start the evm pool
    let handle = evm_pool.spawn(tx_receiver, mock_evaluator, deadline);

    // Send some transactions
    for _ in 0..5 {
        let test_tx = Arc::new(SimTxEnvelope(new_test_tx(&test_wallet).unwrap()));
        println!("sending tx in {:?}", test_tx.0);
        tx_sender.send(test_tx).unwrap();
    }

    // Wait for the handle to complete
    let best = handle.await.unwrap();

    // Check the result
    assert!(best.is_some());
    assert_eq!(best.unwrap().score, U256::from(1));
}

fn mock_evaluator(_state: &ResultAndState) -> U256 {
    U256::from(1)
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
