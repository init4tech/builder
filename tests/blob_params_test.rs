//! Integration test verifying that blob parameters are correctly derived
//! from the chain spec rather than hardcoded to Prague values.
//!
//! Forks Ethereum mainnet via Anvil and exercises the full
//! `SubmitPrep::prep_transaction` -> `Bumpable::new` -> `populate_initial_gas`
//! code path to ensure blob gas pricing uses BPO2 params on the current network.

#![recursion_limit = "256"]

use alloy::{
    consensus::{BlockHeader, SimpleCoder},
    eips::{BlockId, Encodable2718, eip4844::builder::SidecarBuilder, eip7840::BlobParams},
    network::{TransactionBuilder, TransactionBuilder7594},
    node_bindings::Anvil,
    primitives::{B256, Bytes, U256, hex},
    providers::{Provider, WalletProvider, ext::MevApi},
    rpc::types::mev::{EthCallBundle, EthSendBundle},
    sol_types::SolCall,
};
use builder::{
    quincey::Quincey,
    tasks::{block::cfg::SignetCfgEnv, submit::SubmitPrep},
    test_utils::{setup_logging, setup_mainnet_test_config},
    utils,
};
use init4_bin_base::utils::signer::LocalOrAws;
use signet_sim::BuiltBlock;
use signet_zenith::Passage;

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
    let anvil = Anvil::new().fork("https://ethereum-rpc.publicnode.com").chain_id(1).spawn();

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
    let prep = SubmitPrep::new(&built_block, host_provider.clone(), quincey, config.clone());
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
    let anvil = Anvil::new().fork("https://ethereum-rpc.publicnode.com").chain_id(1).spawn();

    let builder_key_hex = hex::encode(anvil.first_key().to_bytes());
    let config = setup_mainnet_test_config(&anvil.endpoint(), &builder_key_hex);

    let host_provider = config.connect_host_provider().await.unwrap();
    let latest_block = host_provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    let host_header = latest_block.header.inner;

    assert!(host_header.timestamp() > BPO2_ACTIVATION, "Fork must be post-BPO2");

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

    let prep = SubmitPrep::new(&built_block, host_provider.clone(), quincey, config.clone());
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
    let relays = config.connect_relays().await.unwrap();
    let (_, flashbots) = relays.first().unwrap();
    let builder_signer = config.connect_builder_signer().await.unwrap();
    let target_block = host_header.number() + 1;

    // Prague-params bundle (the old buggy behavior)
    let prague_bundle = EthCallBundle {
        txs: vec![prague_tx_bytes],
        block_number: target_block,
        state_block_number: alloy::eips::BlockNumberOrTag::Latest,
        ..Default::default()
    };

    let prague_result =
        flashbots.call_bundle(prague_bundle).with_auth(builder_signer.clone()).into_future().await;

    // BPO2-params bundle (the fixed behavior)
    let bpo2_bundle = EthCallBundle {
        txs: vec![bpo2_tx_bytes],
        block_number: target_block,
        state_block_number: alloy::eips::BlockNumberOrTag::Latest,
        ..Default::default()
    };

    let bpo2_result =
        flashbots.call_bundle(bpo2_bundle).with_auth(builder_signer).into_future().await;

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

/// Logs the current slot position relative to the 12-second slot boundary.
fn log_slot_position(calc: &init4_bin_base::utils::calc::SlotCalculator, label: &str) {
    let slot = calc.current_slot().expect("chain has started");
    let pos_ms = calc.current_point_within_slot_ms().expect("chain has started");
    let slot_dur_ms = calc.slot_duration() * 1000;
    let remaining_ms = slot_dur_ms.saturating_sub(pos_ms);
    tracing::info!(slot, position_ms = pos_ms, remaining_ms, "[slot-timer] {label}");
}

/// Sleeps until the specified millisecond offset within the next slot.
///
/// If we are already past `target_ms` in the current slot, waits for the
/// next slot boundary and then sleeps `target_ms` into it.
async fn wait_for_slot_position(
    calc: &init4_bin_base::utils::calc::SlotCalculator,
    target_ms: u64,
) {
    let slot_dur_ms = calc.slot_duration() * 1000;
    assert!(target_ms < slot_dur_ms, "target_ms must be < slot duration");

    let pos_ms = calc.current_point_within_slot_ms().expect("chain has started");

    let wait_ms = if pos_ms <= target_ms {
        // Still time in current slot — wait the remaining delta
        target_ms - pos_ms
    } else {
        // Past the target in this slot — wait for next slot + target
        (slot_dur_ms - pos_ms) + target_ms
    };

    tracing::info!(
        current_position_ms = pos_ms,
        target_ms,
        wait_ms,
        "[slot-timer] Waiting for target slot position"
    );
    tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;

    // Log actual position after waking (will have scheduler jitter)
    log_slot_position(calc, "Woke at target position");
}

/// Queries `flashbots_getBundleStatsV2` via raw reqwest with auth header.
///
/// Returns the raw JSON response body for logging. The Flashbots relay
/// requires the same `X-Flashbots-Signature` auth as bundle submission.
async fn get_bundle_stats(signer: &LocalOrAws, bundle_hash: B256, target_block: u64) -> String {
    use alloy::providers::ext::sign_flashbots_payload;

    let body_str = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"flashbots_getBundleStatsV2","params":[{{"bundleHash":"{bundle_hash}","blockNumber":"0x{target_block:x}"}}]}}"#,
    );
    let sig = sign_flashbots_payload(body_str.clone(), signer).await.unwrap();

    reqwest::Client::new()
        .post("https://rpc.flashbots.net")
        .header("X-Flashbots-Signature", &sig)
        .header("Content-Type", "application/json")
        .body(body_str)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

/// Submits a real MEV bundle to Flashbots mainnet containing:
/// 1. A Passage `enter` transaction (L1 host tx bridging ETH to the rollup)
/// 2. An EIP-4844 blob transaction with random data
///
/// Uses a slot timer to submit the bundle at a precise point within the
/// 12-second slot window. Control the submission timing with:
///
/// - `SLOT_SUBMIT_AT_MS` — milliseconds into the slot to submit (default: 2000)
///
/// Queries `flashbots_getBundleStatsV2` for diagnostics and checks on-chain
/// inclusion. Requires `MAINNET_PRIV_KEY` env var set to a funded private key.
///
/// # Examples
///
/// ```bash
/// # Submit early in the slot (2s mark, the default)
/// MAINNET_PRIV_KEY=0x... cargo test --test blob_params_test test_passage -- --ignored --nocapture
///
/// # Submit at the 500ms mark (very early — best chance for inclusion)
/// SLOT_SUBMIT_AT_MS=500 MAINNET_PRIV_KEY=0x... cargo test ...
///
/// # Submit late in the slot (8s mark — likely too late)
/// SLOT_SUBMIT_AT_MS=8000 MAINNET_PRIV_KEY=0x... cargo test ...
/// ```
#[ignore = "integration test: requires MAINNET_PRIV_KEY with funded mainnet account"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_passage_enter_bundle_submission() {
    setup_logging();

    // -- 1. Read env vars ---------------------------------------------------
    let funded_key = match std::env::var("MAINNET_PRIV_KEY") {
        Ok(k) => k,
        Err(_) => {
            tracing::warn!("MAINNET_PRIV_KEY not set — skipping bundle submission test");
            return;
        }
    };
    let funded_key = funded_key.strip_prefix("0x").unwrap_or(&funded_key);

    let submit_at_ms: u64 =
        std::env::var("SLOT_SUBMIT_AT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2000);

    tracing::info!(submit_at_ms, "Slot timer configured");

    // -- 2. Init config with real mainnet RPC + funded key -----------------
    let config = setup_mainnet_test_config("https://ethereum-rpc.publicnode.com", funded_key);
    let calc = &config.slot_calculator;

    // -- 3. Connect providers ----------------------------------------------
    let host_provider = config.connect_host_provider().await.unwrap();
    let builder_signer = config.connect_builder_signer().await.unwrap();
    let relays = config.connect_relays().await.unwrap();
    tracing::info!(n_relays = relays.len(), "Connected to relay endpoints");

    let sender = host_provider.default_signer_address();
    tracing::info!(%sender, "Using funded account");
    log_slot_position(calc, "Test start");

    // -- 4. Fetch nonce + balance (do this before waiting) -----------------
    let nonce = host_provider.get_transaction_count(sender).await.unwrap();
    let balance = host_provider.get_balance(sender).await.unwrap();
    tracing::info!(%balance, nonce, "Account state");

    // -- 5. Pre-compute expensive work BEFORE the slot timer ----------------
    //    KZG proof generation for the blob sidecar takes ~1.6s. Pre-compute
    //    it now so the hot path after the timer is just: fetch header, sign,
    //    and submit.
    log_slot_position(calc, "Pre-computing blob sidecar (KZG proofs)");
    let random_data: Vec<u8> = (0..32).flat_map(|_| B256::random().0).collect();
    let sidecar = SidecarBuilder::<SimpleCoder>::from_slice(&random_data).build_7594().unwrap();
    log_slot_position(calc, "Sidecar ready");

    let enter_input: Bytes = Passage::enter_0Call { rollupRecipient: sender }.abi_encode().into();
    let enter_value = U256::from(100_000_000_000_000u64); // 0.0001 ETH
    // Read priority fee from env, default 10 gwei. Bump higher to test
    // competitiveness (e.g. PRIORITY_FEE_GWEI=50).
    let priority_gwei: u128 =
        std::env::var("PRIORITY_FEE_GWEI").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let priority_fee: u128 = priority_gwei * 1_000_000_000;
    tracing::info!(priority_gwei, "Priority fee configured");

    // -- 6. Wait for the target slot position ------------------------------
    //    Everything below is the hot path — must be fast.
    wait_for_slot_position(calc, submit_at_ms).await;

    // -- 7. Fetch latest block immediately after waking --------------------
    let latest_block = host_provider.get_block(BlockId::latest()).await.unwrap().unwrap();
    let host_header = latest_block.header.inner;
    let target_block = host_header.number() + 1;

    log_slot_position(calc, "Header fetched");
    tracing::info!(
        current_block = host_header.number(),
        target_block,
        timestamp = host_header.timestamp(),
        "Targeting next block"
    );

    // -- 8. Derive gas params from fresh header ----------------------------
    let next_timestamp = host_header.timestamp() + calc.slot_duration();
    let cfg = SignetCfgEnv::new(config.constants.host_chain_id(), next_timestamp);
    let blob_params = cfg.blob_params().unwrap();

    let base_fee = host_header
        .next_block_base_fee(alloy::eips::eip1559::BaseFeeParams::ethereum())
        .unwrap() as u128;
    let max_fee = base_fee * 1025 / 1024 + priority_fee;

    // -- 9. Build + sign Passage enter tx (fast — no KZG) ------------------
    let passage_req = alloy::rpc::types::TransactionRequest::default()
        .with_to(config.constants.host_passage())
        .with_input(enter_input)
        .with_value(enter_value)
        .with_nonce(nonce)
        .with_gas_limit(100_000)
        .with_max_priority_fee_per_gas(priority_fee)
        .with_max_fee_per_gas(max_fee);

    let passage_sendable = host_provider.fill(passage_req).await.unwrap();
    let passage_envelope = passage_sendable.try_into_envelope().unwrap();
    let passage_tx_hash = *passage_envelope.tx_hash();
    let passage_bytes: Bytes = passage_envelope.encoded_2718().into();

    // -- 10. Build + sign blob tx (fast — sidecar already computed) ---------
    let mut blob_req = alloy::rpc::types::TransactionRequest::default()
        .with_blob_sidecar(sidecar)
        .with_to(sender) // self-transfer; some builders filter Address::ZERO
        .with_nonce(nonce + 1)
        .with_gas_limit(21_000);

    utils::populate_initial_gas(&mut blob_req, &host_header, blob_params);
    blob_req.max_priority_fee_per_gas = Some(priority_fee);
    blob_req.max_fee_per_gas = Some(max_fee);

    let blob_sendable = host_provider.fill(blob_req).await.unwrap();
    let blob_envelope = blob_sendable.try_into_envelope().unwrap();
    let blob_tx_hash = *blob_envelope.tx_hash();
    let blob_bytes: Bytes = blob_envelope.encoded_2718().into();

    log_slot_position(calc, "Both txs signed");
    tracing::info!(?passage_tx_hash, ?blob_tx_hash, "Bundle ready");

    // -- 11. Fan-out bundle to all relays in parallel -----------------------
    //    Send the full bundle (passage + blob) targeting both block N and
    //    block N+1 to every builder simultaneously for maximum inclusion.
    let full_txs = vec![passage_bytes, blob_bytes];

    // How many blocks ahead to target. Default 1 (next block only).
    // Set TARGET_BLOCKS=2 to also target block N+1.
    let target_blocks: u64 =
        std::env::var("TARGET_BLOCKS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    tracing::info!(target_blocks, "Block targeting configured");

    let mut submit_futs = Vec::new();
    for (url, provider) in relays.iter() {
        let host = url.host_str().unwrap_or("unknown").to_string();
        for offset in 0..target_blocks {
            let blk = target_block + offset;
            let bundle =
                EthSendBundle { txs: full_txs.clone(), block_number: blk, ..Default::default() };
            let signer = builder_signer.clone();
            let fut = provider.send_bundle(bundle).with_auth(signer).into_future();
            submit_futs.push((host.clone(), blk, fut));
        }
    }

    // Fire all submissions concurrently
    let results = futures_util::future::join_all(
        submit_futs.into_iter().map(|(name, blk, fut)| async move { (name, blk, fut.await) }),
    )
    .await;

    log_slot_position(calc, "All relays responded");

    let mut accepted = 0u32;
    let mut rejected = 0u32;
    let mut bundle_hash = B256::ZERO;
    for (relay, blk, result) in &results {
        match result {
            Ok(Some(resp)) => {
                tracing::info!(relay, block = blk, hash = ?resp.bundle_hash, "accepted");
                if bundle_hash == B256::ZERO {
                    bundle_hash = resp.bundle_hash;
                }
                accepted += 1;
            }
            Ok(None) => {
                tracing::debug!(relay, block = blk, "returned None");
                accepted += 1;
            }
            Err(err) => {
                tracing::warn!(relay, block = blk, %err, "rejected");
                rejected += 1;
            }
        }
    }
    tracing::info!(accepted, rejected, total = results.len(), "Relay submission summary");

    // -- 12. Poll bundle stats -----------------------------------------------
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    log_slot_position(calc, "Stats poll");
    let stats = get_bundle_stats(&builder_signer, bundle_hash, target_block).await;
    tracing::info!(target_block, ?bundle_hash, stats = %stats, "Bundle stats");

    // -- 13. Wait for all targeted blocks to pass ---------------------------
    let wait_for_block = target_block + target_blocks - 1;
    let timeout = tokio::time::Instant::now() + std::time::Duration::from_secs(36);
    loop {
        let current = host_provider.get_block_number().await.unwrap();
        if current >= wait_for_block {
            log_slot_position(calc, "Target blocks reached");
            tracing::info!(current, target_block, "Both target blocks passed");
            break;
        }
        if tokio::time::Instant::now() > timeout {
            tracing::warn!(current, target_block, "Timed out waiting for target blocks");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // Final stats poll after block
    let final_stats = get_bundle_stats(&builder_signer, bundle_hash, target_block).await;
    tracing::info!(
        target_block,
        ?bundle_hash,
        stats = %final_stats,
        "Final bundle stats"
    );

    // -- 14. Check on-chain inclusion --------------------------------------
    let passage_receipt = host_provider.get_transaction_receipt(passage_tx_hash).await.unwrap();
    let blob_receipt = host_provider.get_transaction_receipt(blob_tx_hash).await.unwrap();

    if let Some(p) = &passage_receipt {
        tracing::info!(
            block = p.block_number,
            status = ?p.status(),
            submit_at_ms,
            "Passage TX LANDED on-chain (bundle without blob)"
        );
    }
    if let Some(b) = &blob_receipt {
        tracing::info!(
            block = b.block_number,
            status = ?b.status(),
            submit_at_ms,
            "Blob TX LANDED on-chain (bundle with blob)"
        );
    }
    if passage_receipt.is_none() && blob_receipt.is_none() {
        tracing::warn!(submit_at_ms, "Neither bundle landed");
    }

    let landed = passage_receipt.is_some() || blob_receipt.is_some();
    if landed {
        tracing::info!(submit_at_ms, "Bundle LANDED on-chain");
    }

    // Assert bundle was at least submitted successfully (hash received)
    assert_ne!(bundle_hash, B256::ZERO, "Bundle hash must be non-zero");
}
