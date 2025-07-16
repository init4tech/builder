# The Signet Block Builder

The Builder simulates bundles and transactions against the latest chain state to create valid Signet rollup blocks and submits them to the configured host chain as an [EIP-4844 transaction](https://github.com/ethereum/EIPs/blob/master/EIPS/eip-4844.md).

Bundles are treated as Flashbots-style bundles, meaning that the Builder should respect transaction ordering, bundle atomicity, and the specified revertability.

---

## 🚀 System Design

The Builder orchestrates a series of asynchronous actors that work together to build blocks for every assigned slot.

1. **Env** - watches the latest host and rollup blocks to monitor gas rates and block updates.
2. **Cache** - polls bundle and transaction caches and adds them to the cache.
3. **Simulator** - simulates transactions and bundles against rollup state and block environment to build them into a cohesive block.
5. **Submit** - creates a blob transaction from the built block and sends it to Ethereum L1.
6. **Metrics** - records block and tx data over time.

```mermaid
%%{ init : { "theme" : "dark" } }%%
flowchart TD
    %% ────────────── INITIALIZATION ──────────────
    A0(["Start main"]) --> A1[Init tracing & logging]
    A1 --> A2_BuilderConfig[Load BuilderConfig from env]
    
    %% ────────────── CORE TASK SPAWNS ──────────────
    subgraph Tasks_Spawned["Spawned Actors"]
        EnvTaskActor["🔢 Env Task"] ==block_env==> CacheSystem
        CacheSystem["🪏 Cache System"]
        MetricsTaskActor["📏 Metrics Task"]
        SubmitTaskActor["📡 Submit Task "]
        SimulatorTaskActor["💾 Simulator Task"]
        Quincey["🖊️ Quincey"]

        SubmitTaskActor -.block hash.-> Quincey
        Quincey -.block signature.-> SubmitTaskActor
    end

    %% ────────────── CONNECTIONS & DATA FLOW ──────────────
    A2_BuilderConfig -.host_provider.-> MetricsTaskActor
    A2_BuilderConfig -.host_provider.->SubmitTaskActor
    A2_BuilderConfig -.ru_provider.-> SimulatorTaskActor
    A2_BuilderConfig -.ru_provider.-> EnvTaskActor

    A3["📥 Transactions &
    📦 Bundles"] --> CacheSystem

    EnvTaskActor ==block_env==> SimulatorTaskActor
    CacheSystem ==sim_cache ==> SimulatorTaskActor
    SubmitTaskActor ==tx receipt==> MetricsTaskActor
    SimulatorTaskActor ==built block==> SubmitTaskActor

    SubmitTaskActor ==>|"signet block (blob tx)"| C1["⛓️ Ethereum L1"]
```

The block building loop waits until a new block has been received, and then kicks off the next attempt. 

When the Builder receives a new block, it takes a reference to the transaction cache, calculates a simulation deadline for the current slot with a buffer of 1.5 seconds, and begins constructing a block for the current slot.

Transactions enter through the cache, and then they're sent to the simulator, where they're run against the latest chain state and block environment. If they're successfully applied, they're added to the block. If a transaction fails to be applied, it is simply ignored. 

When the deadline is reached, the simulator is stopped, and all open simulation threads and cancelled. The block is then bundled with the block environment and the previous host header that it was simulated against, and passes all three along to the submit task. 

If no transactions in the cache are valid and the resulting block is empty, the submit task will ignore it. 

Finally, if it's non-empty, the submit task attempts to get a signature for the block, and if it fails due to a 403 error, it will skip the current slot and begin waiting for the next block. 

---

## ⚙️ Configuration

The Builder is configured via environment variables. The following values are supported for configuration.

| Key                           | Required | Description                                                         |
| ----------------------------- | -------- | ------------------------------------------------------------------- |
| `HOST_CHAIN_ID`               | Yes      | Host-chain ID (e.g. `3151908`)                                      |
| `RU_CHAIN_ID`                 | Yes      | Rollup-chain ID (e.g. `14174`)                                      |
| `HOST_RPC_URL`                | Yes      | RPC endpoint for the host chain                                     |
| `ROLLUP_RPC_URL`              | Yes      | RPC endpoint for the rollup chain                                   |
| `TX_POOL_URL`                 | Yes      | Transaction pool URL (must end with `/`)                            |
| `TX_BROADCAST_URLS`           | No       | Additional endpoints for blob txs (comma-separated, slash required) |
| `ZENITH_ADDRESS`              | Yes      | Zenith contract address                                             |
| `BUILDER_HELPER_ADDRESS`      | Yes      | Builder helper contract address                                     |
| `QUINCEY_URL`                 | Yes      | Remote sequencer signing endpoint                                   |
| `BUILDER_PORT`                | Yes      | HTTP port for the Builder (default: `8080`)                         |
| `SEQUENCER_KEY`               | Yes      | AWS KMS key ID _or_ local private key for sequencer signing         |
| `BUILDER_KEY`                 | Yes      | AWS KMS key ID _or_ local private key for builder signing           |
| `BUILDER_REWARDS_ADDRESS`     | Yes      | Address receiving builder rewards                                   |
| `ROLLUP_BLOCK_GAS_LIMIT`      | No       | Override for block gas limit                                        |
| `CONCURRENCY_LIMIT`           | Yes      | Max concurrent tasks the simulator uses                             |
| `OAUTH_CLIENT_ID`             | Yes      | Oauth client ID for the builder                                     |
| `OAUTH_CLIENT_SECRET`         | Yes      | Oauth client secret for the builder                                 |
| `OAUTH_AUTHENTICATE_URL`      | Yes      | Oauth authenticate URL for the builder for performing OAuth logins  |
| `OAUTH_TOKEN_URL`             | Yes      | Oauth token URL for the builder to get an Oauth2 access token       |
| `AUTH_TOKEN_REFRESH_INTERVAL` | Yes      | The OAuth token refresh interval in seconds.                        |
| `SLOT_OFFSET`                 | Yes      | Slot timing offset in seconds                                       |
| `SLOT_DURATION`               | Yes      | Slot duration in seconds                                            |
| `START_TIMESTAMP`             | Yes      | UNIX timestamp for slot 0                                           |

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

---

## 📤 Transaction Sender

A binary (`bin/submit-transaction.rs`) for continously sending very small transactions for testing block construction.

The following values are available for configuring the transaction sender:

| Key                 | Required | Description                                      |
| ------------------- | -------- | ------------------------------------------------ |
| `RPC_URL`           | Yes      | RPC endpoint used for sending the transaction    |
| `RECIPIENT_ADDRESS` | Yes      | Address to which the transaction is sent         |
| `SLEEP_TIME`        | No       | Optional delay (in seconds) between transactions |
| `SIGNER_CHAIN_ID`   | Yes      | Chain ID used for signing                        |
| `SIGNER_KEY`        | Yes      | Signing key used to sign the transaction         |

The transaction submitter is located at `bin/submit_transaction.rs`.

Run the transaction submitter with `cargo run --bin transaction-submitter`

---

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
