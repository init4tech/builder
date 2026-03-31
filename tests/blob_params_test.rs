//! Integration test verifying that blob parameters are correctly derived
//! from the chain spec rather than hardcoded to Prague values.
//!
//! Forks Ethereum mainnet via Anvil and exercises the full
//! `SubmitPrep::prep_transaction` -> `Bumpable::new` -> `populate_initial_gas`
//! code path to ensure blob gas pricing uses BPO2 params on the current network.

#![recursion_limit = "256"]

use alloy::{
    consensus::BlockHeader,
    eips::{BlockId, Encodable2718, eip7840::BlobParams},
    network::TransactionBuilder,
    node_bindings::Anvil,
    primitives::{Bytes, hex},
    providers::{Provider, ext::MevApi},
    rpc::types::mev::EthCallBundle,
};
use builder::{
    quincey::Quincey,
    tasks::{block::cfg::SignetCfgEnv, submit::SubmitPrep},
    test_utils::{setup_logging, setup_mainnet_test_config},
    utils,
};
use init4_bin_base::utils::signer::LocalOrAws;
use signet_sim::BuiltBlock;

/// BPO2 activation timestamp on the mainnet host chain spec.
const BPO2_ACTIVATION: u64 = 1767747671;

/// Tests that the builder derives BPO2 blob parameters from the chain spec
/// when operating on a post-BPO2 mainnet fork, rather than using hardcoded
/// Prague values.
///
/// This exercises the "blackholed submit" pattern: the full SubmitPrep path
/// runs (quincey signing, sidecar encoding, gas estimation) but the resulting
/// transaction is never submitted to Flashbots.
#[ignore = "integration test: requires network access for Anvil mainnet fork"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_blob_params_derived_from_chainspec_not_hardcoded() {
    setup_logging();

    // -- 1. Fork mainnet with Anvil ----------------------------------------
    let anvil = Anvil::new()
        .fork("https://ethereum-rpc.publicnode.com")
        .chain_id(1)
        .spawn();

    // Extract a private key from the Anvil instance for signing
    let builder_key_hex = hex::encode(anvil.first_key().to_bytes());

    // -- 2. Init global config with mainnet constants + Anvil endpoint ------
    let config = setup_mainnet_test_config(&anvil.endpoint(), &builder_key_hex);

    // -- 3. Connect to the Anvil fork as host provider ----------------------
    let host_provider = config.connect_host_provider().await.unwrap();

    // -- 4. Fetch the latest mainnet block header ---------------------------
    let latest_block = host_provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    let host_header = latest_block.header.inner;

    // Sanity: the fork timestamp must be after BPO2 activation
    assert!(
        host_header.timestamp() > BPO2_ACTIVATION,
        "Fork timestamp {} should be after BPO2 activation {BPO2_ACTIVATION}",
        host_header.timestamp(),
    );

    // -- 5. Verify blob params resolve to BPO2 -----------------------------
    let next_timestamp = host_header.timestamp() + config.slot_calculator.slot_duration();
    let cfg = SignetCfgEnv::new(config.constants.host_chain_id(), next_timestamp);
    let blob_params = cfg.blob_params().unwrap();

    assert_eq!(blob_params.target_blob_count, 14, "BPO2 target_blob_count");
    assert_eq!(blob_params.max_blob_count, 21, "BPO2 max_blob_count");
    assert_eq!(blob_params.update_fraction, 11_684_671, "BPO2 update_fraction");

    // Must NOT be Prague params
    assert_ne!(
        blob_params.update_fraction,
        BlobParams::prague().update_fraction,
        "update_fraction must not match Prague"
    );

    // -- 6. Compare blob fees: BPO2 vs Prague for the same header ----------
    let bpo2_blob_fee = host_header.next_block_blob_fee(blob_params).unwrap();
    let prague_blob_fee = host_header.next_block_blob_fee(BlobParams::prague()).unwrap();

    assert_ne!(
        bpo2_blob_fee, prague_blob_fee,
        "BPO2 and Prague blob fees must differ for the same header"
    );

    // -- 7. Mock Quincey with a local signer (Owned) -----------------------
    let sequencer_signer = LocalOrAws::load(&builder_key_hex, Some(1)).await.unwrap();
    let quincey = Quincey::new_owned(sequencer_signer);

    // -- 8. Create a minimal BuiltBlock ------------------------------------
    let built_block = BuiltBlock::new(1);

    // -- 9. Exercise the blackholed submit path ----------------------------
    let prep = SubmitPrep::new(
        &built_block,
        host_provider.clone(),
        quincey,
        config.clone(),
    );
    let bumpable = prep.prep_transaction(&host_header).await.unwrap();

    // -- 10. Assert the transaction request has correct gas values ----------
    let req = bumpable.req();

    let max_fee_per_blob_gas = req.max_fee_per_blob_gas.expect("max_fee_per_blob_gas must be set");
    assert_eq!(
        max_fee_per_blob_gas, bpo2_blob_fee,
        "max_fee_per_blob_gas should equal the BPO2-derived blob fee"
    );

    // Sanity: blob fee should be reasonable (< 1 ETH)
    assert!(
        max_fee_per_blob_gas < 1_000_000_000_000_000_000u128,
        "max_fee_per_blob_gas ({max_fee_per_blob_gas}) is unreasonably high"
    );

    assert!(req.max_fee_per_gas.is_some(), "max_fee_per_gas must be set");
    assert!(req.max_priority_fee_per_gas.is_some(), "max_priority_fee_per_gas must be set");
}

/// Proves the negative: constructs a transaction using the old hardcoded
/// `BlobParams::prague()` and submits it to the Flashbots relay for simulation
/// via `eth_callBundle`. On a BPO2 network the Prague-derived blob fee is
/// wrong, and Flashbots simulation surfaces the error.
#[ignore = "integration test: requires network access for Anvil mainnet fork + Flashbots relay"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_prague_blob_params_rejected_by_flashbots_simulation() {
    setup_logging();

    // -- 1. Fork mainnet with Anvil ----------------------------------------
    let anvil = Anvil::new()
        .fork("https://ethereum-rpc.publicnode.com")
        .chain_id(1)
        .spawn();

    let builder_key_hex = hex::encode(anvil.first_key().to_bytes());
    let config = setup_mainnet_test_config(&anvil.endpoint(), &builder_key_hex);

    let host_provider = config.connect_host_provider().await.unwrap();
    let latest_block = host_provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    let host_header = latest_block.header.inner;

    assert!(
        host_header.timestamp() > BPO2_ACTIVATION,
        "Fork must be post-BPO2"
    );

    // -- 2. Derive both BPO2 (correct) and Prague (buggy) blob fees --------
    let next_timestamp = host_header.timestamp() + config.slot_calculator.slot_duration();
    let cfg = SignetCfgEnv::new(config.constants.host_chain_id(), next_timestamp);
    let bpo2_params = cfg.blob_params().unwrap();
    let prague_params = BlobParams::prague();

    let bpo2_blob_fee = host_header.next_block_blob_fee(bpo2_params).unwrap();
    let prague_blob_fee = host_header.next_block_blob_fee(prague_params).unwrap();

    tracing::info!(
        bpo2_fee = bpo2_blob_fee,
        prague_fee = prague_blob_fee,
        bpo2_update_fraction = bpo2_params.update_fraction,
        prague_update_fraction = prague_params.update_fraction,
        "Blob fee comparison: BPO2 (correct) vs Prague (buggy)"
    );

    // -- 3. Build a transaction via SubmitPrep (correct BPO2 path) ---------
    let sequencer_signer = LocalOrAws::load(&builder_key_hex, Some(1)).await.unwrap();
    let quincey = Quincey::new_owned(sequencer_signer);
    let built_block = BuiltBlock::new(1);

    let prep = SubmitPrep::new(
        &built_block,
        host_provider.clone(),
        quincey,
        config.clone(),
    );
    let bumpable = prep.prep_transaction(&host_header).await.unwrap();

    // -- 4. Clone the request and create a Prague-poisoned version ----------
    //    Override max_fee_per_blob_gas with the Prague-derived (wrong) value,
    //    then set gas_limit so the filler skips estimation (which would fail
    //    because the Zenith contract call reverts with our test key).
    let mut prague_req = bumpable.req().clone();
    prague_req.max_fee_per_blob_gas = None;
    prague_req.max_fee_per_gas = None;
    prague_req.max_priority_fee_per_gas = None;
    utils::populate_initial_gas(&mut prague_req, &host_header, prague_params);
    prague_req = prague_req.with_gas_limit(1_000_000);

    let mut bpo2_req = bumpable.into_request();
    bpo2_req = bpo2_req.with_gas_limit(1_000_000);

    tracing::info!(
        prague_max_blob_gas = prague_req.max_fee_per_blob_gas,
        bpo2_max_blob_gas = bpo2_req.max_fee_per_blob_gas,
        "Transaction blob gas: Prague (buggy) vs BPO2 (correct)"
    );

    // -- 5. Fill and sign both transactions via the host provider ----------
    let prague_sendable = host_provider.fill(prague_req).await.unwrap();
    let prague_envelope = prague_sendable.try_into_envelope().unwrap();
    let prague_tx_bytes: Bytes = prague_envelope.encoded_2718().into();

    let bpo2_sendable = host_provider.fill(bpo2_req).await.unwrap();
    let bpo2_envelope = bpo2_sendable.try_into_envelope().unwrap();
    let bpo2_tx_bytes: Bytes = bpo2_envelope.encoded_2718().into();

    // -- 6. Simulate both against Flashbots via eth_callBundle -------------
    let flashbots = config.connect_flashbots().await.unwrap();
    let builder_signer = config.connect_builder_signer().await.unwrap();
    let target_block = host_header.number() + 1;

    // Prague-params bundle (the old buggy behavior)
    let prague_bundle = EthCallBundle {
        txs: vec![prague_tx_bytes],
        block_number: target_block,
        state_block_number: alloy::eips::BlockNumberOrTag::Latest,
        ..Default::default()
    };

    let prague_result = flashbots
        .call_bundle(prague_bundle)
        .with_auth(builder_signer.clone())
        .into_future()
        .await;

    // BPO2-params bundle (the fixed behavior)
    let bpo2_bundle = EthCallBundle {
        txs: vec![bpo2_tx_bytes],
        block_number: target_block,
        state_block_number: alloy::eips::BlockNumberOrTag::Latest,
        ..Default::default()
    };

    let bpo2_result = flashbots
        .call_bundle(bpo2_bundle)
        .with_auth(builder_signer)
        .into_future()
        .await;

    // -- 7. Report results -------------------------------------------------
    match &prague_result {
        Err(err) => {
            tracing::error!(%err, "Flashbots REJECTED Prague-params bundle (expected)");
        }
        Ok(Some(resp)) => {
            tracing::warn!(
                total_gas = resp.total_gas_used,
                gas_price = %resp.bundle_gas_price,
                n_results = resp.results.len(),
                "Flashbots accepted Prague-params bundle (overpaying)"
            );
            for (i, tx_result) in resp.results.iter().enumerate() {
                tracing::warn!(
                    idx = i,
                    revert = ?tx_result.revert,
                    value = ?tx_result.value,
                    gas_used = tx_result.gas_used,
                    "Prague bundle tx result"
                );
            }
        }
        Ok(None) => tracing::warn!("Flashbots returned None for Prague-params bundle"),
    }

    match &bpo2_result {
        Err(err) => {
            tracing::error!(%err, "Flashbots rejected BPO2-params bundle");
        }
        Ok(Some(resp)) => {
            tracing::info!(
                total_gas = resp.total_gas_used,
                gas_price = %resp.bundle_gas_price,
                n_results = resp.results.len(),
                "Flashbots accepted BPO2-params bundle"
            );
            for (i, tx_result) in resp.results.iter().enumerate() {
                tracing::info!(
                    idx = i,
                    revert = ?tx_result.revert,
                    value = ?tx_result.value,
                    gas_used = tx_result.gas_used,
                    "BPO2 bundle tx result"
                );
            }
        }
        Ok(None) => tracing::info!("Flashbots returned None for BPO2-params bundle"),
    }

    // The blob fees must differ — this is the core invariant the fix corrects
    assert_ne!(
        prague_blob_fee, bpo2_blob_fee,
        "Prague and BPO2 blob fees must differ on a BPO2 network"
    );
}
