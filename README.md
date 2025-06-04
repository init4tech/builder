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

# Transaction Pool Configs
TX_POOL_URL="http://pool.url.here/" # trailing slash is required

# RPC Endpoints
HOST_RPC_URL="https://host-rpc.pecorino.signet.sh"
RU_RPC_URL="https://rpc.pecorino.signet.sh"
TX_POOL_URL=""
TX_BROADCAST_URLS="" # trailing slash is required - set to none for test net configuration

# Contract configurations
ZENITH_ADDRESS="0xbe45611502116387211D28cE493D6Fb3d192bc4E"
BUILDER_HELPER_ADDRESS="0xb393416A722Fd48C3b0a9ab5fC2512Ef0c55e4CA"

# Misc.
QUINCEY_URL="http://sequencer.pecorino.signet.sh/signBlock"
BUILDER_PORT="8080"
SEQUENCER_KEY=""
BUILDER_KEY=""
BUILDER_REWARDS_ADDRESS="BUILDER_REWARDS_ADDRESS_HERE"
ROLLUP_BLOCK_GAS_LIMIT=""
CONCURRENCY_LIMIT=""

# Pecorino Slot Timing Configurations
SLOT_OFFSET="4"
SLOT_DURATION="12"
START_TIMESTAMP="1740681556"
```

## Transaction Sender

The builder includes a `transaction-sender` for sending miniscule transactions for the purpose of testing the rollup block construction process. The `transaction-sender` is located in `bin/submit-transaction.rs`.

It requires a key to sign the transactions and a funded wallet.

### Environment Variables 

The `transaction-sender` also has a set of configurable environment variables listed below.

```sh
RPC_URL="" # The URL of the RPC endpoint of the node you're sending the transaction to.
RECIPIENT_ADDRESS="" # The address the submitter addresses the transaction to.
SLEEP_TIME="" # The time to wait before sending another transaction, in seconds.
SIGNER_CHAIN_ID=""
SIGNER_KEY=""
```

## Testing

### F

In order to test the `builder`, one must build and deploy an image in the target network with a provisioned key. 

1. Build a docker image with your changes 
2. Push the image to the image repository
3. Update the image in the deployment
4. Assert and confirm expected behavior

