# Signet Block Builder Development Guide

## Crate Summary

Single-crate Rust application (not a workspace) that builds Signet rollup blocks. Actor-based async task system: watches host/rollup chains, ingests transactions and bundles, simulates them against rollup state, then submits valid blocks to Ethereum as EIP-4844 blob transactions via Flashbots. Built on alloy, trevm, tokio, and the `signet-*` SDK crates. Binary: `zenith-builder-example`. Minimum Rust 1.88, Edition 2024.

## Build Commands

```bash
make              # Debug build
make release      # Optimized release build
make run          # Run zenith-builder-example binary
make test         # Run all tests
make fmt          # Format code
make clippy       # Lint with warnings denied
```

Always lint before committing. The Makefile provides shortcuts (`make fmt`, `make clippy`, `make test`)

### Running Individual Tests

```bash
cargo test test_name                    # Run specific test by name
cargo test --test test_file_name        # Run all tests in a specific test file
cargo test -- --ignored                 # Run ignored integration tests (require network)
```

## Architecture

Five actor tasks communicate via tokio channels:

1. **EnvTask** (`src/tasks/env.rs`) - Subscribes to rollup blocks, fetches matching host headers, runs Quincey preflight slot check, constructs `SimEnv` (host + rollup `BlockEnv`), broadcasts via `watch` channel.
2. **CacheTasks** (`src/tasks/cache/`) - `TxPoller` and `BundlePoller` ingest transactions/bundles into a shared `SimCache`.
3. **SimulatorTask** (`src/tasks/block/sim.rs`) - Receives `SimEnv`, clones the cache, builds a `BlockBuild` with a slot-derived deadline, produces `SimResult`.
4. **FlashbotsTask** (`src/tasks/submit/flashbots.rs`) - Receives `SimResult`, prepares signed EIP-4844 blob transaction via `SubmitPrep` + Quincey, bundles with host txs, submits to Flashbots relay.
5. **MetricsTask** (`src/tasks/metrics.rs`) - Tracks tx mining status and records metrics.

**Data flow:** `EnvTask → (watch) → SimulatorTask ← (SimCache) ← CacheTasks` `SimulatorTask → (mpsc) → FlashbotsTask → Quincey → Flashbots`

### Source Layout

```
bin/
  builder.rs          - Binary entry point, spawns all tasks, select! on join handles
src/
  lib.rs              - Crate root, global CONFIG OnceLock, lint directives
  config.rs           - BuilderConfig (FromEnv), provider type aliases, connect_* methods
  quincey.rs          - Quincey enum (Remote/Owned), signing + preflight
  service.rs          - Axum /healthcheck endpoint
  macros.rs           - span_scoped!, span_debug/info/warn/error!, res/opt_unwrap_or_continue!
  utils.rs            - Signature extraction, gas population helpers
  test_utils.rs       - setup_test_config, new_signed_tx, test_block_env helpers
  tasks/
    mod.rs            - Module re-exports
    env.rs            - EnvTask, SimEnv, Environment types
    block/
      mod.rs          - Module re-exports
      sim.rs          - SimulatorTask, SimResult, block building + deadline calc
      cfg.rs          - SignetCfgEnv for simulation
    cache/
      mod.rs          - Module re-exports
      task.rs         - CacheTask
      tx.rs           - TxPoller
      bundle.rs       - BundlePoller
      system.rs       - CacheSystem, CacheTasks orchestration
    submit/
      mod.rs          - Module re-exports
      flashbots.rs    - FlashbotsTask, bundle preparation + submission
      prep.rs         - SubmitPrep (tx preparation + Quincey signing), Bumpable
      sim_err.rs      - SimErrorResp, SimRevertKind
    metrics.rs        - MetricsTask
```

## Repo Conventions

- Global static config: `CONFIG: OnceLock<BuilderConfig>` initialized via `config_from_env()`. Tasks access config via `crate::config()`.
- Provider type aliases: `HostProvider`, `RuProvider`, `FlashbotsProvider`, `ZenithInstance` are defined in `config.rs` and used throughout.
- `connect_*` methods on `BuilderConfig` use `OnceCell`/`OnceLock` for memoization -- providers and signers are connected once, then cloned.
- Internal macros: `span_scoped!`, `span_debug/info/warn/error!` log within an unentered span. `res_unwrap_or_continue!` and `opt_unwrap_or_continue!` unwrap-or-log-and-continue in loops.
- Quincey has two modes: `Remote` (HTTP/OAuth for production) and `Owned` (local/AWS KMS for dev). Configured by presence of `SEQUENCER_KEY` env var.
- Tasks follow a `new() -> spawn()` pattern: `new()` connects providers, `spawn()` returns channel endpoints + `JoinHandle`.
- Block simulation uses `trevm` with `concurrent-db` and `AlloyDB` backed by alloy providers.
- EIP-4844 blob encoding uses `SimpleCoder` and the 7594 sidecar builder.

## init4 Organization Style

### Research

- Prefer building crate docs (`cargo doc`) and reading them over grepping.

### Code Style

- Functional combinators over imperative control flow. No unnecessary nesting.
- Terse Option/Result handling: `option.map(Thing::do_something)` or `let Some(a) = option else { return; };`.
- Small, focused functions and types.
- Never add incomplete code. No `TODO`s for core logic.
- Never use glob imports. Group imports from the same crate. No blank lines between imports.
- Visibility: private by default, `pub(crate)` for internal, `pub` for API. Never use `pub(super)`.

### Error Handling

- `thiserror` for library errors. Never `anyhow`. `eyre` is allowed in this binary crate but not in library code.
- Propagate with `?` and `map_err`.

### Tracing

- Use `tracing` crate. Instrument work items, not long-lived tasks.
- `skip(self)` when instrumenting methods. Add only needed fields.
- Levels: TRACE (rare, verbose), DEBUG (sparingly), INFO (default), WARN (potential issues), ERROR (prevents operation).
- Propagate spans through task boundaries with `Instrument`.
- This crate uses `span_scoped!` macros to log within unentered spans.

### Async

- Tokio multi-thread runtime. No blocking in async functions.
- Long-lived tasks: return a spawnable future via `spawn()`, don't run directly.
- Short-lived spawned tasks: consider span propagation with `.instrument()`.

### Testing

- Tests panic, never return `Result`. Use `unwrap()` directly.
- Use `setup_test_config()` from `test_utils` to initialize the global config.
- Unit tests in `mod tests` at file bottom. Integration tests in `tests/`.

### Rustdoc

- Doc all public items. Include usage examples in rustdoc.
- Hide scaffolding with `#`. Keep examples concise.
- Traits must include an implementation guide.

### GitHub

- Fresh branches off `main` for PRs. Descriptive branch names.
- AI-authored GitHub comments must include `**[Claude Code]**` header. Minimum: 1.85, Edition: 2024

## Testing

### Integration Tests

Most tests in `tests/` are marked `#[ignore]` and require network access (real RPC endpoints or Anvil).

### Simulation Harness (Offline Tests)

`src/test_utils/` provides a testing harness for offline simulation testing:

- `TestDbBuilder` - Create in-memory EVM state
- `TestSimEnvBuilder` - Create `RollupEnv`/`HostEnv` without RPC
- `TestBlockBuildBuilder` - Build blocks with `BlockBuild`
- `basic_scenario()`, `gas_limit_scenario()` - Pre-configured test scenarios

## Workflow

After completing a set of changes, always run `make fmt` and `make clippy` and fix any issues before committing.

## Local Development

For local SDK development, uncomment the `[patch.crates-io]` section in Cargo.toml to point to local signet-sdk paths.
