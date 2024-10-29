# builder

Our sample signet builder implementation.

## Development

This crate contains an example block builder in the Signet ecosystem.

### Requirements

- Rust 1.81.0
- Cargo [Lambda](https://www.cargo-lambda.info/)
- AWS CLI and credentials

### Environment

The following environment variables are exposed to configure the Builder:

```bash
# Builder Configs
HOST_CHAIN_ID="17000" # Holesky Testnet
RU_CHAIN_ID="17001"
HOST_RPC_URL="http://host.url.here"
# trailing slash is required
TX_BROADCAST_URLS="http://tx.broadcast.url.here/,https://additional.url.here/"
ZENITH_ADDRESS="ZENITH_ADDRESS_HERE"
QUINCEY_URL="http://signer.url.here"
BUILDER_PORT="8080"
BUILDER_KEY="YOUR_BUILDER_KEY_HERE"
INCOMING_TRANSACTIONS_BUFFER="10"
BLOCK_CONFIRMATION_BUFFER="10"
BUILDER_REWARDS_ADDRESS="BUILDER_REWARDS_ADDRESS_HERE"
ROLLUP_BLOCK_GAS_LIMIT="30000000"
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
