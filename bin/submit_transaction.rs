use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, U256},
    providers::{Provider as _, ProviderBuilder, WalletProvider},
    rpc::types::eth::TransactionRequest,
};
use builder::config::HostProvider;
use init4_bin_base::{
    deps::{
        metrics::{counter, histogram},
        tracing,
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

        ProviderBuilder::new()
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
    tracing::trace!("connecting to provider");
    let provider = config.provider().await;
    let recipient_address = config.recipient_address;
    let sleep_time = config.sleep_time;

    loop {
        tracing::debug!("attempting transaction");
        send_transaction(&provider, recipient_address).await;

        tracing::debug!(sleep_time, "sleeping");
        tokio::time::sleep(tokio::time::Duration::from_secs(sleep_time)).await;
    }
}

async fn send_transaction(provider: &HostProvider, recipient_address: Address) {
    // construct simple transaction to send ETH to a recipient
    let tx = TransactionRequest::default()
        .with_from(provider.default_signer_address())
        .with_to(recipient_address)
        .with_value(U256::from(1))
        .with_gas_limit(30_000);

    // start timer to measure how long it takes to mine the transaction
    let dispatch_start_time: Instant = Instant::now();

    // dispatch the transaction
    tracing::debug!("dispatching transaction");
    let result = provider.send_transaction(tx).await.unwrap();

    // wait for the transaction to mine
    let receipt = match timeout(Duration::from_secs(240), result.get_receipt()).await {
        Ok(Ok(receipt)) => receipt,
        Ok(Err(e)) => {
            tracing::error!(error = ?e, "failed to get transaction receipt");
            return;
        }
        Err(_) => {
            tracing::error!("timeout waiting for transaction receipt");
            counter!("txn_submitter.tx_timeout").increment(1);
            return;
        }
    };

    let hash = receipt.transaction_hash.to_string();

    // record metrics for how long it took to mine the transaction
    let mine_time = dispatch_start_time.elapsed().as_secs();
    tracing::debug!(success = receipt.status(), mine_time, hash, "transaction mined");
    histogram!("txn_submitter.tx_mine_time").record(mine_time as f64);
}
