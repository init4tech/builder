use alloy::{primitives::U256, signers::local::PrivateKeySigner};
use builder::test_utils::{new_signed_tx, setup_logging, setup_test_config};
use eyre::{Ok, Result};
use futures_util::TryStreamExt;
use signet_tx_cache::TxCache;

#[ignore = "integration test"]
#[tokio::test]
async fn test_tx_roundtrip() -> Result<()> {
    setup_logging();
    setup_test_config();

    // Post a transaction to the cache
    post_tx().await?;

    // Fetch transactions from the pool
    let tx_cache = TxCache::new(builder::config().tx_pool_url.clone());
    let transactions: Vec<_> = tx_cache.stream_transactions().try_collect().await?;

    // Ensure at least one transaction exists
    assert!(!transactions.is_empty());

    Ok(())
}

async fn post_tx() -> Result<()> {
    let client = reqwest::Client::new();

    let wallet = PrivateKeySigner::random();
    let tx_envelope = new_signed_tx(&wallet, 1, U256::from(1), 10_000)?;

    let url = format!("{}/transactions", builder::config().tx_pool_url);
    let response = client.post(&url).json(&tx_envelope).send().await?;

    if !response.status().is_success() {
        let error_text = response.text().await?;
        eyre::bail!("Failed to post transaction: {}", error_text);
    }

    Ok(())
}
