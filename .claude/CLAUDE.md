# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Signet Block Builder - a Rust application that simulates transactions and bundles against rollup state to create valid Signet rollup blocks, then submits them to Ethereum as EIP-4844 blob transactions via Flashbots.

## Build Commands

```bash
make              # Debug build
make release      # Optimized release build
make run          # Run zenith-builder-example binary
make test         # Run all tests
make fmt          # Format code
make clippy       # Lint with warnings denied
```

Equivalent cargo commands work directly (`cargo build`, `cargo test`, etc.).

## Architecture

The builder uses an actor-based async task system with five main tasks communicating via channels:

1. **EnvTask** (`src/tasks/env.rs`) - Watches host/rollup blocks, maintains block environment state via watch channel
2. **CacheTasks** (`src/tasks/cache/`) - TxPoller and BundlePoller ingest transactions/bundles into SimCache
3. **SimulatorTask** (`src/tasks/block/sim.rs`) - Simulates txs/bundles against rollup state, builds blocks with 1.5s buffer deadline
4. **FlashbotsTask** (`src/tasks/submit/flashbots.rs`) - Signs blocks via Quincey, submits to Flashbots relay
5. **MetricsTask** (`src/tasks/metrics.rs`) - Tracks tx mining status and records metrics

**Data flow:** EnvTask → (block_env) → SimulatorTask ← (SimCache) ← CacheTasks; SimulatorTask → (built block) → FlashbotsTask → Quincey → Flashbots

## Key Components

- **Quincey** (`src/quincey.rs`): Block signing client - supports remote (HTTP/OAuth) or local (AWS KMS) signing
- **BuilderConfig** (`src/config.rs`): Environment variable loading, provider connections
- **Service** (`src/service.rs`): HTTP server with `/healthcheck` endpoint

## Dependencies

Key external crates:
- `signet-*`: SDK crates for constants, simulation, tx-cache, types, zenith contract
- `alloy`: Ethereum interaction (providers, signers, EIP-4844)
- `trevm`: EVM simulator with concurrent-db
- `tokio`: Async runtime

## Configuration

Required environment variables: `HOST_RPC_URL`, `ROLLUP_RPC_URL`, `QUINCEY_URL`, `TX_POOL_URL`, `BUILDER_KEY`, `BUILDER_REWARDS_ADDRESS`, `BUILDER_PORT`, `BLOCK_QUERY_CUTOFF_BUFFER`, plus OAuth settings (`OAUTH_CLIENT_ID`, `OAUTH_CLIENT_SECRET`, `OAUTH_AUTHENTICATE_URL`, `OAUTH_TOKEN_URL`, `AUTH_TOKEN_REFRESH_INTERVAL`).

Optional: `SEQUENCER_KEY` for local signing instead of remote Quincey, `CONCURRENCY_LIMIT` for parallel simulation threads.

## Rust Version

Minimum: 1.88, Edition: 2024

## Local Development

For local SDK development, uncomment the `[patch.crates-io]` section in Cargo.toml to point to local signet-sdk paths.
