# The Signet Block Builder

The Builder simulates bundles and transactions against the latest chain state to create valid Signet rollup blocks and submits them to the configured host chain as an [EIP-4844 transaction](https://github.com/ethereum/EIPs/blob/master/EIPS/eip-4844.md).

Bundles are treated as Flashbots-style bundles, meaning that the Builder should respect transaction ordering, bundle atomicity, and the specified revertability.

---

## 🚀 System Design

The Builder orchestrates a set of asynchronous actors that collaborate to build and submit Signet rollup blocks for every assigned slot. The core components are:

1. **Env** — watches host and rollup chain headers and maintains the block environment used for simulation.
2. **Cache** — ingests transactions and bundles from configured sources and exposes a simulation view.
3. **Simulator** — applies transactions and bundles against the rollup state and block environment to assemble a candidate Signet block.
4. **Submit** — takes a successfully built block and routes it to the configured submission path (either a private MEV bundle relay or direct L1 broadcast via the builder helper).
5. **Metrics** — records runtime metrics, submit receipts, and block statistics.

Below is a high-level architecture diagram that shows both submission options: direct L1 submission (Builder Helper) and private MEV bundle submission (Flashbots relay).

```mermaid
%%{ init : { "theme" : "dark", "flowchart": {"curve":"basis"} } }%%
flowchart TD
    %% Initialization
    start(["Start main"]) --> init["Init tracing & logging"]
    init --> cfg["Load BuilderConfig from env"]

    %% Spawned actors
    subgraph Actors["Spawned Actors"]
        Env["🔢 Env Task"]
        Cache["🪏 Cache System"]
        Simulator["� Simulator Task"]
        SubmitBH["📡 Submit Task (BuilderHelper)"]
        SubmitFB["�️ Submit Task (Flashbots)"]
        Metrics["📏 Metrics Task"]
        Quincey["🖊️ Quincey (Signer)"]
    end

    %% Config wiring
    cfg -.-> Metrics
    cfg -.-> SubmitBH
    cfg -.-> Simulator
    cfg -.-> Env
    cfg -. "flashbots_endpoint" -> SubmitFB

    %% Data flow
    inbound["📥 Transactions & Bundles"] --> Cache
    Env ==block_env==> Simulator
    Cache ==sim_cache==> Simulator
    Simulator ==built_block==> SubmitBH
    Simulator ==built_block==> SubmitFB

    SubmitBH ==tx_receipt==> Metrics
    SubmitFB ==bundle_receipt==> Metrics

    %% Signing interactions
    SubmitBH -.block_hash.-> Quincey
    Quincey -.block_signature.-> SubmitBH
    SubmitFB -.bundle_hash.-> Quincey
    Quincey -.bundle_signature.-> SubmitFB

    %% External targets
    SubmitBH -->|"signet block (blob tx)"| L1["⛓️ Ethereum L1"]
    SubmitFB -->|"MEV bundle"| Relay["🛡️ Flashbots Relay"]

    classDef ext fill:#111,stroke:#bbb,color:#fff;
    class L1,Relay ext
```

### Simulation Task

The simulation loop waits for a new block environment from the host chain, then starts a simulation window for the current slot. The Builder takes a reference to the transaction cache and computes a deadline (default: slot end minus 1.5s buffer) to stop simulating and finalize the block.

Transactions flow from the cache into the simulator. Applied transactions are kept and assembled into the candidate block; failing transactions are ignored. When the simulation deadline arrives, the simulator cancels remaining work, captures the built block and the block environment it was simulated against, and forwards them to the Submit task.

### Submit Task

The Submit task will route the built block to one of two paths, depending on configuration:

- Flashbots (private MEV relay): when `FLASHBOTS_ENDPOINT` is configured, the Submit task prepares an MEV-style bundle containing the Signet block (plus any host transactions/fills as needed) and submits it to the configured relay. The builder expects a bundle hash in the response and records it in metrics.

- Builder Helper (direct L1 broadcast): when no Flashbots endpoint is configured, the Submit task composes a builder-helper contract call and broadcasts it to the mempool as a normal transaction. This path is intended for testing and private deployments; broadcasting raw Signet block data publicly may leak sensitive information.

If the simulated block is empty, the Submit task will drop it and continue.

If the submit path requires a signature (Quincey), the submit task requests one; on an authorization failure (e.g. 403) the submit task will skip the slot and resume on the next.

---

## ⚙️ Configuration

The Builder is configured via environment variables. The following values are supported for configuration.

| Key                           | Required | Description                                                            |
| ----------------------------- | -------- | ---------------------------------------------------------------------- |
| `HOST_CHAIN_ID`               | Yes      | Host-chain ID (e.g. `3151908`)                                         |
| `RU_CHAIN_ID`                 | Yes      | Rollup-chain ID (e.g. `14174`)                                         |
| `HOST_RPC_URL`                | Yes      | RPC endpoint for the host chain                                        |
| `ROLLUP_RPC_URL`              | Yes      | RPC endpoint for the rollup chain                                      |
| `TX_POOL_URL`                 | Yes      | Transaction pool URL (must end with `/`)                               |
| `TX_BROADCAST_URLS`           | No       | Additional endpoints for blob txs (comma-separated, slash required)    |
| `FLASHBOTS_ENDPOINT`          | No       | Flashbots API to submit blocks to.                                     |
| `ZENITH_ADDRESS`              | Yes      | Zenith contract address                                                |
| `BUILDER_HELPER_ADDRESS`      | Yes      | Builder helper contract address                                        |
| `QUINCEY_URL`                 | Yes      | Remote sequencer signing endpoint                                      |
| `BUILDER_PORT`                | Yes      | HTTP port for the Builder (default: `8080`)                            |
| `SEQUENCER_KEY`               | Yes      | AWS KMS key ID _or_ local private key for sequencer signing            |
| `BUILDER_KEY`                 | Yes      | AWS KMS key ID _or_ local private key for builder signing              |
| `BUILDER_REWARDS_ADDRESS`     | Yes      | Address receiving builder rewards                                      |
| `ROLLUP_BLOCK_GAS_LIMIT`      | No       | Override for block gas limit                                           |
| `CONCURRENCY_LIMIT`           | No       | Max concurrent tasks the simulator uses                                |
| `OAUTH_CLIENT_ID`             | Yes      | Oauth client ID for the builder                                        |
| `OAUTH_CLIENT_SECRET`         | Yes      | Oauth client secret for the builder                                    |
| `OAUTH_AUTHENTICATE_URL`      | Yes      | Oauth authenticate URL for the builder for performing OAuth logins     |
| `OAUTH_TOKEN_URL`             | Yes      | Oauth token URL for the builder to get an Oauth2 access token          |
| `AUTH_TOKEN_REFRESH_INTERVAL` | Yes      | The OAuth token refresh interval in seconds.                           |
| `CHAIN_NAME`                  | No       | The chain name ("pecorino", or the corresponding name)                 |
| `SLOT_OFFSET`                 | No       | Slot timing offset in seconds. Required if `CHAIN_NAME` is not present |
| `SLOT_DURATION`               | No       | Slot duration in seconds. Required if `CHAIN_NAME` is not present      |
| `START_TIMESTAMP`             | No       | UNIX timestamp for slot 0. Required if `CHAIN_NAME` is not present     |

---

## 💻 Recommended Specs

| Key                | Minimum            | Recommended       |
| ------------------ | ------------------ | ----------------- |
| CPU                | 0.1 vCPU           | 0.5 vCPU          |
| Memory             | 256MB              | 512MB             | 

**Note: Builder prefers clock speed over core count, recommended 2.8Ghz+**

---

## 💾 EVM Behavior

### 🗿 Inherited Header Values

`PREVRANDAO` is set to a random byte string for each block.

```rust
// `src/tasks/env.rs`
prevrandao: Some(B256::random()),
```

`TIMESTAMP` - Block timestamps are set to the same value as the current Ethereum block.

Blob gas values `excess_blob_gas` and `blob_gasprice` are also set to 0 for all Signet blocks.

### 🔢 Disabled Opcodes 

`BLOBHASH` - EIP-4844 is not supported on Signet.
`BLOBBASEFEE` - EIP4844 is not supported.

## ⛽ Transaction Submission

When a completed, non-empty Signet block is received by the Submit task, it prepares the block data into a blob transaction and submits it to the network. 

If it fails, it will retry up to 3 times with a 12.5% bump on each retry. 

The previous header's basefee is tracked through the build loop and used for gas estimation purposes in the Submit Task.

## 🛠️ Development

### Requirements

- **Rust** ≥ 1.85
- **AWS CLI**
- A private key or AWS KMS key for signing transactions

---

## ✅ Testing

1. Build the Docker image:
   ```bash
   docker build -t builder:latest .
   ```
2. Push to your container registry:
   ```bash
   docker push <registry>/builder:latest
   ```
3. Update your deployment manifests with the new image.
4. Verify expected behavior in your target network.
   - This should typically include sending a test transaction and verifying it is simulated and built into a block.
  
## 🪪 License

This project is licensed under the [MIT License](https://opensource.org/licenses/MIT).
