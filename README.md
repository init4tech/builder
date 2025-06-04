# builder

Our sample signet builder implementation.

## Development

This crate contains an example block builder in the Signet ecosystem.

### Requirements

- Rust 1.85
- AWS CLI 
- A private key or AWS KMS key for signing transactions

### Environment

The following environment variables are exposed to configure the Builder:

```bash
# Builder Configs for Pecorino Test Net
HOST_CHAIN_ID="3151908"
RU_CHAIN_ID="14174"
HOST_RPC_URL="https://host-rpc.pecorino.signet.sh"
TX_BROADCAST_URLS="" # trailing slash is required - set to none for test net configuration
ZENITH_ADDRESS="0xbe45611502116387211D28cE493D6Fb3d192bc4E"
QUINCEY_URL="http://sequencer.pecorino.signet.sh/signBlock"
BUILDER_PORT="8080"
INCOMING_TRANSACTIONS_BUFFER="10"
BLOCK_CONFIRMATION_BUFFER="10"
BUILDER_REWARDS_ADDRESS="BUILDER_REWARDS_ADDRESS_HERE"
ROLLUP_BLOCK_GAS_LIMIT="30000000"
CONCURRENCY_LIMIT=10 # Concurrency parameter for simulation
# Pecorino Slot Timing Configuration
SLOT_OFFSET="4"
SLOT_DURATION="12"
START_TIMESTAMP="1740681556"
# Transaction Pool Configs
TX_POOL_URL="http://pool.url.here/" # trailing slash is required
TX_POOL_POLL_INTERVAL="5" # seconds
TX_POOL_CACHE_DURATION="600" # seconds
```

## API

### SignRequest

Sign request example payload:

```json
{
  "hostBlockNumber": "0x0",
  "hostChainId": "0x1",
  "ruChainId": "0x2",
  "gasLimit": "0x5",
  "ruRewardAddress": "0x0606060606060606060606060606060606060606",
  "contents": "0x0707070707070707070707070707070707070707070707070707070707070707"
}
```
 
## Transaction Sender

The builder includes a `transaction-sender` for sending miniscule transactions for the purpose of testing the rollup block construction process. The `transaction-sender` is located in `bin/submit-transaction.rs`.

It requires a key to sign the transactions and a funded wallet.

### Environment Variables 

The `transaction-sender` also has a set of configurable environment variables listed below.

```
RPC_URL="" # The URL of the RPC endpoint of the node you're sending the transaction to.
RECIPIENT_ADDRESS="" # The address the submitter addresses the transaction to.
SLEEP_TIME="" # The time to wait before sending another transaction, in seconds.
SIGNER_CHAIN_ID=""
SIGNER_KEY="" # 
```