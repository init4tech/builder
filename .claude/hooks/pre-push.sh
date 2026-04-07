#!/usr/bin/env bash
set -euo pipefail

# Consume stdin (hook protocol)
cat > /dev/null

cd "$(dirname "$0")/../.."

echo "Running pre-push checks..." >&2

if ! make fmt 2>&1; then
    echo "Format check failed. Run 'make fmt' to fix." >&2
    exit 2
fi

if ! make clippy 2>&1; then
    echo "Clippy failed." >&2
    exit 2
fi

if ! RUSTDOCFLAGS="-D warnings" cargo doc --no-deps 2>&1; then
    echo "Doc check failed." >&2
    exit 2
fi

echo "All pre-push checks passed." >&2
exit 0
