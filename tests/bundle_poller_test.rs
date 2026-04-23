#![cfg(feature = "test-utils")]

use builder::test_utils::{setup_logging, setup_test_config};
use eyre::Result;
use futures_util::TryStreamExt;
use init4_bin_base::perms::tx_cache::BuilderTxCache;

#[tokio::test]
async fn test_bundle_poller_roundtrip() -> Result<()> {
    setup_logging();
    setup_test_config();

    let config = builder::config();
    let tx_cache = BuilderTxCache::new(config.tx_pool_url.clone(), config.oauth_token());

    let _bundles: Vec<_> = tx_cache.stream_bundles().try_collect().await?;

    Ok(())
}
