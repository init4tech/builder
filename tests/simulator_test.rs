use std::str::FromStr;
use std::sync::Arc;
use alloy::consensus::{SignableTransaction as _, TxEip1559, TxEnvelope};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync as _;
use builder::tasks::simulator::{EvmPool, SimTxEnvelope, SimulatorFactory};
use revm::db::CacheDB;
use revm::primitives::{Address, TxKind};
use revm::InMemoryDB;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use alloy::primitives::U256;
use trevm::{NoopBlock, NoopCfg};
use trevm::revm::primitives::ResultAndState;

#[tokio::test]
async fn test_spawn() {
    let test_wallet = PrivateKeySigner::random();

    let (tx_sender, tx_receiver) = mpsc::unbounded_channel::<Arc<SimTxEnvelope>>();
    let deadline = Instant::now() + Duration::from_secs(5);

    let db = CacheDB::new(InMemoryDB::default());
    let ext = ();

    let evm_factory = SimulatorFactory::new(db, ext);
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
        to: TxKind::Call(
            Address::from_str("0x0000000000000000000000000000000000000000").unwrap(),
        ),
        value: U256::from(1_f64),
        input: alloy::primitives::bytes!(""),
        ..Default::default()
    };
    let signature = wallet.sign_hash_sync(&tx.signature_hash())?;
    Ok(TxEnvelope::Eip1559(tx.into_signed(signature)))
}
