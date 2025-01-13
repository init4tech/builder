use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, U256},
    providers::{Provider as _, ProviderBuilder, WalletProvider},
    rpc::types::eth::TransactionRequest,
    signers::aws::AwsSigner,
};
use aws_config::BehaviorVersion;
use builder::config::{load_address, load_string, load_u64, load_url, Provider};
use metrics::counter;
use metrics::histogram;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::time::{Duration, Instant};
use tokio::time::timeout;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::try_init().unwrap();

    tracing::trace!("installing metrics collector");
    PrometheusBuilder::new().install().expect("failed to install prometheus exporter");

    tracing::trace!("connecting to provider");
    let (provider, recipient_address, sleep_time) = connect_from_config().await;

    loop {
        tracing::debug!("attempting transaction");
        send_transaction(provider.clone(), recipient_address).await;

        tracing::debug!(sleep_time, "sleeping");
        tokio::time::sleep(tokio::time::Duration::from_secs(sleep_time)).await;
    }
}

async fn send_transaction(provider: Provider, recipient_address: Address) {
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
    histogram!("integration.tx_mine_time").record(mine_time as f64);
}

async fn connect_from_config() -> (Provider, Address, u64) {
    // load signer config values from .env
    let rpc_url = load_url("RPC_URL").unwrap();
    let chain_id = load_u64("CHAIN_ID").unwrap();
    let kms_key_id = load_string("AWS_KMS_KEY_ID").unwrap();
    // load transaction sending config value from .env
    let recipient_address: Address = load_address("RECIPIENT_ADDRESS").unwrap();
    let sleep_time = load_u64("SLEEP_TIME").unwrap();

    // connect signer & provider
    let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let client = aws_sdk_kms::Client::new(&config);
    let signer = AwsSigner::new(client, kms_key_id.to_string(), Some(chain_id)).await.unwrap();

    let provider = ProviderBuilder::new()
        .with_recommended_fillers()
        .wallet(EthereumWallet::from(signer))
        .on_builtin(&rpc_url)
        .await
        .unwrap();

    (provider, recipient_address, sleep_time)
}
