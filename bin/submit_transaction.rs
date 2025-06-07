//! A simple transaction submitter that sends a transaction to a recipient address
//! on a regular interval for the purposes of roughly testing rollup mining.
use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, U256},
    providers::{
        Provider as _, ProviderBuilder, WalletProvider,
        fillers::{BlobGasFiller, SimpleNonceManager},
    },
    rpc::types::eth::TransactionRequest,
};
use builder::config::HostProvider;
use init4_bin_base::{
    deps::{
        metrics::{counter, histogram},
        tracing::{debug, error},
    },
    init4,
    utils::{from_env::FromEnv, signer::LocalOrAwsConfig},
};
use std::time::{Duration, Instant};
use tokio::time::timeout;

#[derive(Debug, Clone, FromEnv)]
struct Config {
    #[from_env(var = "RPC_URL", desc = "Ethereum RPC URL")]
    rpc_url: String,
    kms_key_id: LocalOrAwsConfig,
    #[from_env(var = "RECIPIENT_ADDRESS", desc = "Recipient address")]
    recipient_address: Address,
    #[from_env(var = "SLEEP_TIME", desc = "Time to sleep between transactions")]
    sleep_time: u64,
}

impl Config {
    async fn provider(&self) -> HostProvider {
        let signer = self.kms_key_id.connect_remote().await.unwrap();

        ProviderBuilder::new_with_network()
            .disable_recommended_fillers()
            .filler(BlobGasFiller)
            .with_gas_estimation()
            .with_nonce_management(SimpleNonceManager::default())
            .fetch_chain_id()
            .wallet(EthereumWallet::from(signer))
            .connect(&self.rpc_url)
            .await
            .unwrap()
    }
}

#[tokio::main]
async fn main() {
    let _guard = init4();

    let config = Config::from_env().unwrap();
    debug!(?config.recipient_address, "connecting to provider");

    let provider = config.provider().await;
    let recipient_address = config.recipient_address;
    let sleep_time = config.sleep_time;

    loop {
        debug!(?recipient_address, "attempting transaction");
        send_transaction(&provider, recipient_address).await;

        debug!(sleep_time, "sleeping");
        tokio::time::sleep(tokio::time::Duration::from_secs(sleep_time)).await;
    }
}

/// Sends a transaction to the specified recipient address
async fn send_transaction(provider: &HostProvider, recipient_address: Address) {
    // construct simple transaction to send ETH to a recipient
    let nonce = match provider.get_transaction_count(provider.default_signer_address()).await {
        Ok(count) => count,
        Err(e) => {
            error!(error = ?e, "failed to get transaction count");
            return;
        }
    };

    let tx = TransactionRequest::default()
        .with_from(provider.default_signer_address())
        .with_to(recipient_address)
        .with_value(U256::from(1))
        .with_nonce(nonce)
        .with_gas_limit(30_000);

    // start timer to measure how long it takes to mine the transaction
    let dispatch_start_time: Instant = Instant::now();

    // dispatch the transaction
    debug!(?tx.nonce, "sending transaction with nonce");
    let result = provider.send_transaction(tx).await.unwrap();

    // wait for the transaction to mine
    let receipt = match timeout(Duration::from_secs(240), result.get_receipt()).await {
        Ok(Ok(receipt)) => receipt,
        Ok(Err(e)) => {
            error!(error = ?e, "failed to get transaction receipt");
            return;
        }
        Err(_) => {
            error!("timeout waiting for transaction receipt");
            counter!("txn_submitter.tx_timeout").increment(1);
            return;
        }
    };

    record_metrics(dispatch_start_time, receipt);
}

/// Record metrics for how long it took to mine the transaction
fn record_metrics(dispatch_start_time: Instant, receipt: alloy::rpc::types::TransactionReceipt) {
    let mine_time = dispatch_start_time.elapsed().as_secs();
    let hash = receipt.transaction_hash.to_string();
    debug!(success = receipt.status(), mine_time, hash, "transaction mined");
    histogram!("txn_submitter.tx_mine_time").record(mine_time as f64);
}
