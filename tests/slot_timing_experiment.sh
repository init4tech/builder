#!/usr/bin/env bash
# Slot timing experiment: measure inclusion rates for blob bundles
# submitted at different points within a 12-second Ethereum slot.
#
# Fans out to: flashbots, titan (3 regions), beaverbuild, lightspeed
#
# Required env:
#   MAINNET_PRIV_KEY  — funded mainnet private key
#
# Optional env:
#   PRIORITY_FEE_GWEI — priority fee in gwei (default: 50)
#   TARGET_BLOCKS     — how many blocks to target: 1 = block N only,
#                       2 = block N and N+1 (default: 1)
#
# Usage:
#   source ~/.zshrc  # load MAINNET_PRIV_KEY
#   ./tests/slot_timing_experiment.sh
#   ./tests/slot_timing_experiment.sh 500 1000 2000 4000 8000
#   TARGET_BLOCKS=2 PRIORITY_FEE_GWEI=100 ./tests/slot_timing_experiment.sh

set -euo pipefail

PRIORITY_FEE_GWEI="${PRIORITY_FEE_GWEI:-50}"
TARGET_BLOCKS="${TARGET_BLOCKS:-1}"
SLOT_TIMES=("${@:-500 1000 2000 3000 4000 5000 6000 7000 8000}")

if [ -z "${MAINNET_PRIV_KEY:-}" ]; then
    echo "ERROR: MAINNET_PRIV_KEY not set" >&2
    exit 1
fi

RESULTS="/tmp/slot_timing_results_$(date +%s).csv"
echo "target_ms,signed_ms,builders_done_ms,accepted,rejected,landed,slot" > "$RESULTS"

echo "============================================================"
echo "  Slot Timing Experiment"
echo "  Priority: ${PRIORITY_FEE_GWEI} gwei"
echo "  Target blocks: ${TARGET_BLOCKS}"
echo "  Slot times: ${SLOT_TIMES[*]}"
echo "============================================================"
echo ""

for ms in ${SLOT_TIMES[@]}; do
    echo "--- SLOT_SUBMIT_AT_MS=${ms} ---"

    RAW=$(RUST_LOG=info \
        SLOT_SUBMIT_AT_MS="$ms" \
        PRIORITY_FEE_GWEI="$PRIORITY_FEE_GWEI" \
        TARGET_BLOCKS="$TARGET_BLOCKS" \
        cargo test --test blob_params_test test_passage_enter_bundle_submission \
        -- --ignored --nocapture 2>&1 | sed $'s/\033\[[0-9;]*m//g') || true

    SIGN_MS=$(echo "$RAW" | grep "Both txs signed" | sed -n 's/.*position_ms=\([0-9]*\).*/\1/p' | head -1)
    DONE_MS=$(echo "$RAW" | grep "All builders responded" | sed -n 's/.*position_ms=\([0-9]*\).*/\1/p' | head -1)
    ACCEPTED=$(echo "$RAW" | grep "Builder submission summary" | sed -n 's/.*accepted=\([0-9]*\).*/\1/p' | head -1)
    REJECTED=$(echo "$RAW" | grep "Builder submission summary" | sed -n 's/.*rejected=\([0-9]*\).*/\1/p' | head -1)
    SLOT=$(echo "$RAW" | grep "Both txs signed" | sed -n 's/.*slot=\([0-9]*\).*/\1/p' | head -1)

    if echo "$RAW" | grep -q "LANDED on-chain"; then LANDED="YES"; else LANDED="no"; fi

    SIGN_MS=${SIGN_MS:-"?"}; DONE_MS=${DONE_MS:-"?"}; ACCEPTED=${ACCEPTED:-"?"}
    REJECTED=${REJECTED:-"?"}; SLOT=${SLOT:-"?"}

    printf "  signed=%4sms  builders=%4sms  accepted=%s  rejected=%s  landed=%-3s  slot=%s\n" \
        "$SIGN_MS" "$DONE_MS" "$ACCEPTED" "$REJECTED" "$LANDED" "$SLOT"
    echo "${ms},${SIGN_MS},${DONE_MS},${ACCEPTED},${REJECTED},${LANDED},${SLOT}" >> "$RESULTS"
done

echo ""
echo "============================================================"
echo "  Results"
echo "============================================================"
echo ""
column -t -s',' "$RESULTS"
echo ""
echo "CSV: $RESULTS"
