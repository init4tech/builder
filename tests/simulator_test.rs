use alloy::{
    consensus::{SignableTransaction as _, TxEip1559, TxEnvelope},
    eips::BlockId,
    genesis::Genesis,
    primitives::U256,
    providers::{Provider, ProviderBuilder},
    signers::{SignerSync as _, local::PrivateKeySigner},
};
use builder::tasks::sim::{SimEnv, SimTask};
use signet_types::config::SignetSystemConstants;
use std::sync::Arc;
use tokio::{sync::mpsc::{self, channel}, time::Duration};
use trevm::{revm::{
    context::result::{ExecutionResult, ResultAndState},
    database::{AlloyDB, CacheDB, InMemoryDB},
    inspector::NoOpInspector,
    primitives::{address, TxKind},
    state::{Account, AccountInfo},
}, Cfg, NoopBlock, NoopCfg};

type AlloyDatabase = AlloyDB<
    alloy::network::Ethereum,
    alloy::providers::fillers::FillProvider<
        alloy::providers::fillers::JoinFill<
            alloy::providers::Identity,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::GasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::BlobGasFiller,
                    alloy::providers::fillers::JoinFill<
                        alloy::providers::fillers::NonceFiller,
                        alloy::providers::fillers::ChainIdFiller,
                    >,
                >,
            >,
        >,
        alloy::providers::RootProvider,
    >,
>;

#[tokio::test(flavor = "multi_thread")]
async fn test_simulate_one() {
    // Setup transaction pipeline plumbing
    let concurrency_limit = 1000;
    let (tx_sender, tx_receiver) = mpsc::channel::<TxEnvelope>(concurrency_limit);
    let (_bundle_sender, _bundle_receiver) = mpsc::unbounded_channel::<Vec<TxEnvelope>>();

    let anvil =
        alloy::node_bindings::Anvil::new().block_time(1).chain_id(14174).try_spawn().unwrap();
    let test_wallet = &PrivateKeySigner::from(anvil.keys()[0].clone());

    let provider = ProviderBuilder::new().on_http(anvil.endpoint_url());
    let block_number = provider.get_block_number().await.unwrap();
    
    let alloy_db = AlloyDB::new(provider, BlockId::from(block_number));

    let evaluator = Arc::new(test_evaluator);

    let test_genesis = Genesis::default(); // TODO: Replace with a real genesis
    let constants = SignetSystemConstants::try_from_genesis(&test_genesis).unwrap();

    let deadline = Duration::from_secs(5);

    // TODO remove noops
    let env: SimEnv<AlloyDatabase, _, _, NoOpInspector> = SimEnv::new(alloy_db, constants, &NoopCfg, &NoopBlock, deadline);

    // Spawn off the actor 
    let actor: SimTask<AlloyDatabase, _, _, _> = SimTask {
        factory: env,
        concurrency_limit,
    };

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
fn new_test_tx(wallet: &PrivateKeySigner, nonce: u64) -> TxEnvelope {
    let tx = TxEip1559 {
        chain_id: 17003,
        gas_limit: 50000,
        nonce,
        to: TxKind::Call(address!("0x0000000000000000000000000000000000000000")),
        value: U256::from(1),
        input: alloy::primitives::bytes!(""),
        ..Default::default()
    };
    let signature = wallet.sign_hash_sync(&tx.signature_hash()).unwrap();
    TxEnvelope::Eip1559(tx.into_signed(signature))
}

// Adds a balance to the given wallet address in the database for simple simulation unit tests
fn seed_database(mut db: CacheDB<InMemoryDB>, wallet: &PrivateKeySigner) -> CacheDB<InMemoryDB> {
    let mut info = AccountInfo::default();
    info.balance = U256::from(10000);
    db.insert_account_info(wallet.address(), info);

    db
}
