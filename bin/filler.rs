use alloy::{
    eips::Encodable2718,
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Bytes, U256, uint},
    providers::{Provider, ProviderBuilder, SendableTx},
    rpc::types::{TransactionRequest, mev::EthSendBundle},
    signers::Signer,
};
use eyre::{Error, eyre};
use std::{collections::HashMap, slice::from_ref};

use builder::config::HostProvider;
use init4_bin_base::utils::{
    from_env::FromEnv,
    signer::{LocalOrAws, LocalOrAwsConfig},
    constants_from_env
};
use signet_bundle::SignetEthBundle;
use signet_constants::SignetConstants;
use signet_tx_cache::{client::TxCache, types::TxCacheSendBundleResponse};
use signet_types::{AggregateOrders, SignedFill, SignedOrder, UnsignedFill, UnsignedOrder};
use signet_zenith::RollupOrders::{Input, Output, Order};
use tokio::time::{Duration, sleep};
use chrono::Utc;

/// Connect a provider capable of filling and sending transactions to a given chain.
pub async fn connect_provider(signer: LocalOrAws, rpc_url: String) -> eyre::Result<HostProvider> {
    ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer))
        .connect(&rpc_url)
        .await
        .map_err(Into::into)
}

/// Configuration for the Filler application.
#[derive(Debug, FromEnv)]
struct FillerConfig {
    /// The Rollup RPC URL.
    #[from_env(var = "RU_RPC_URL", desc = "RPC URL for the Rollup")]
    ru_rpc_url: String,
    /// The signer to use for signing transactions.
    /// NOTE: For the example, this key must be funded with USDC on both the Host and Rollup, as well as gas on the Rollup.
    /// .env vars: SIGNER_KEY, SIGNER_CHAIN_ID
    signer_config: LocalOrAwsConfig,
    /// The Signet constants.
    /// .env var: CHAIN_NAME
    constants: SignetConstants,
}

/// Run the Filler application.
#[tokio::main(flavor = "multi_thread")]
async fn main() -> eyre::Result<()> {
    // load config from environment variables
    let config = FillerConfig::from_env()?;

    // connect signer and provider
    let signer = config.signer_config.connect().await?;
    let provider = connect_provider(signer.clone(), config.ru_rpc_url).await?;

    // SEND AN EXAMPLE ORDER
    let send_order = SendOrder::new(signer.clone(), config.constants)?;
    // get an example Order - 1 rollup USDC for 1 host USDC
    let example_order = send_order.example_order();
    // sign and send it to tx cache
    send_order.sign_and_send_order(example_order).await?;

    // wait ~1 sec to ensure Order is in cache
    sleep(Duration::from_secs(1)).await;

    // FILL THE EXAMPLE ORDER
    let filler = Filler::new(signer, provider, config.constants)?;
    // get all Orders from tx cache
    let orders = filler.get_orders().await?;
    // filter for example Orders only
    let fillable_orders = orders.iter().filter(|o| send_order.is_example_order(o)).collect();
    // fill each Order individually
    filler.fill_individually(orders.as_slice()).await?;

    Ok(())
}

// -------------- COPIED FROM EXAMPLE CODE IN SDK --------------

/// Multiplier for converting gwei to wei.
const GWEI_TO_WEI: u64 = 1_000_000_000;

const ONE_USDC: U256 = uint!(1_000_000_U256);

/// Example code demonstrating API usage and patterns for Signet Fillers.
#[derive(Debug)]
pub struct Filler<S: Signer> {
    /// The signer to use for signing transactions.
    signer: S,
    /// The provider to use for building transactions on the Rollup.
    ru_provider: HostProvider,
    /// The transaction cache endpoint.
    tx_cache: TxCache,
    /// The system constants.
    constants: SignetConstants,
}

impl<S> Filler<S>
where
    S: Signer,
{
    /// Create a new Filler with the given signer, provider, and transaction cache endpoint.
    pub fn new(
        signer: S,
        ru_provider: HostProvider,
        constants: SignetConstants,
    ) -> Result<Self, Error> {
        Ok(Self {
            signer,
            ru_provider,
            tx_cache: TxCache::new_from_string(constants.environment().transaction_cache())?,
            constants,
        })
    }

    /// Query the transaction cache to get all possible orders.
    pub async fn get_orders(&self) -> Result<Vec<SignedOrder>, Error> {
        self.tx_cache.get_orders().await
    }

    /// Fills Orders individually, by submitting a separate Bundle for each Order.
    ///
    /// Filling Orders individually ensures that even if some Orders are not fillable, others may still mine;
    /// however, it is less gas efficient.
    ///
    /// A nice feature of filling Orders individually is that Fillers could be less concerned
    /// about carefully simulating Orders onchain before attempting to fill them.
    /// As long as an Order is economically a "good deal" for the Filler, they can attempt to fill it
    /// without simulating to check whether it has already been filled, because they can rely on Builder simulation.
    /// Order `initiate` transactions will revert if the Order has already been filled,
    /// in which case the entire Bundle would simply be discarded by the Builder.
    pub async fn fill_individually(&self, orders: &[SignedOrder]) -> Result<(), Error> {
        // submit one bundle per individual order
        for order in orders {
            self.fill(from_ref(order)).await?;
        }

        Ok(())
    }

    /// Fills one or more Order(s) in a single, atomic Bundle.
    /// - Signs Fill(s) for the Order(s)
    /// - Constructs a Bundle of transactions to fill & initiate the Order(s)
    /// - Sends the Bundle to the transaction cache to be mined by Builders
    ///
    /// If more than one Order is passed to this fn,
    /// Filling them in aggregate means that Fills are batched and more gas efficient;
    /// however, if a single Order cannot be filled, then the entire Bundle will not mine.
    /// For example, using this strategy, if one Order is filled by another Filler first, then all other Orders will also not be filled.
    ///
    /// If a single Order is passed to this fn,
    /// Filling Orders individually ensures that even if some Orders are not fillable, others may still mine;
    /// however, it is less gas efficient.
    pub async fn fill(&self, orders: &[SignedOrder]) -> Result<TxCacheSendBundleResponse, Error> {
        // if orders is empty, error out
        if orders.is_empty() {
            eyre::bail!("no orders to fill")
        }

        // sign a SignedFill for the orders
        let mut signed_fills = self.sign_fills(orders).await?;

        // get the transaction requests for the rollup
        let tx_requests = self.rollup_txn_requests(&signed_fills, orders).await?;

        // sign & encode the transactions for the Bundle
        let txs = self.sign_and_encode_txns(tx_requests).await?;

        // get the aggregated host fill for the Bundle, if any
        let host_fills = signed_fills.remove(&self.constants.host().chain_id());

        // set the Bundle to only be valid if mined in the next rollup block
        let block_number = self.ru_provider.get_block_number().await? + 1;

        // construct a Bundle containing the Rollup transactions and the Host fill (if any)
        let bundle = SignetEthBundle {
            host_fills,
            bundle: EthSendBundle {
                txs,
                reverting_tx_hashes: vec![], // generally, if the Order initiations revert, then fills should not be submitted
                block_number,
                min_timestamp: None, // sufficiently covered by pinning to next block number
                max_timestamp: None, // sufficiently covered by pinning to next block number
                replacement_uuid: None, // optional if implementing strategies that replace or cancel bundles
            },
        };

        // submit the Bundle to the transaction cache
        self.tx_cache.forward_bundle(bundle).await
    }

    /// Aggregate the given orders into a SignedFill, sign it, and
    /// return a HashMap of SignedFills for each destination chain.
    ///
    /// This is the simplest, minimally viable way to turn a set of SignedOrders into a single Aggregated Fill on each chain;
    /// Fillers may wish to implement more complex setups.
    ///
    /// For example, if utilizing different signers for each chain, they may use `UnsignedFill.sign_for(chain_id)` instead of `sign()`.
    ///
    /// If filling multiple Orders, they may wish to utilize one Order's Outputs to provide another Order's rollup Inputs.
    /// In this case, the Filler would wish to split up the Fills for each Order,
    /// rather than signing a single, aggregate a Fill for each chain, as is done here.
    async fn sign_fills(&self, orders: &[SignedOrder]) -> Result<HashMap<u64, SignedFill>, Error> {
        //  create an AggregateOrder from the SignedOrders they want to fill
        let agg: AggregateOrders = orders.iter().collect();
        // produce an UnsignedFill from the AggregateOrder
        let mut unsigned_fill = UnsignedFill::from(&agg);
        // populate the Order contract addresses for each chain
        for chain_id in agg.destination_chain_ids() {
            unsigned_fill = unsigned_fill.with_chain(
                chain_id,
                self.constants
                    .system()
                    .orders_for(chain_id)
                    .ok_or(eyre!("invalid target chain id {}", chain_id))?,
            );
        }
        // sign the UnsignedFill, producing a SignedFill for each target chain
        Ok(unsigned_fill.sign(&self.signer).await?)
    }

    /// Construct a set of transaction requests to be submitted on the rollup.
    ///
    /// Perform a single, aggregate Fill upfront, then Initiate each Order.
    /// Transaction requests look like [`fill_aggregate`, `initiate_1`, `initiate_2`].
    ///
    /// This is the simplest, minimally viable way to get a set of Orders mined;
    /// Fillers may wish to implement more complex strategies.
    ///
    /// For example, Fillers might utilize one Order's Inputs to fill subsequent Orders' Outputs.
    /// In this case, the rollup transactions should look like [`fill_1`, `inititate_1`, `fill_2`, `initiate_2`].
    async fn rollup_txn_requests(
        &self,
        signed_fills: &HashMap<u64, SignedFill>,
        orders: &[SignedOrder],
    ) -> Result<Vec<TransactionRequest>, Error> {
        // construct the transactions to be submitted to the Rollup
        let mut tx_requests = Vec::new();

        // first, if there is a SignedFill for the Rollup, add a transaction to submit the fill
        // Note that `fill` transactions MUST be mined *before* the corresponding Order(s) `initiate` transactions in order to cound
        // Host `fill` transactions are always considered to be mined "before" the rollup block is processed,
        // but Rollup `fill` transactions MUST take care to be ordered before the Orders are `initiate`d
        if let Some(rollup_fill) = signed_fills.get(&self.constants.rollup().chain_id()) {
            // add the fill tx to the rollup txns
            let ru_fill_tx = rollup_fill.to_fill_tx(self.constants.rollup().orders());
            tx_requests.push(ru_fill_tx);
        }

        // next, add a transaction to initiate each SignedOrder
        for signed_order in orders {
            // add the initiate tx to the rollup txns
            let ru_initiate_tx = signed_order
                .to_initiate_tx(self.signer.address(), self.constants.rollup().orders());
            tx_requests.push(ru_initiate_tx);
        }

        Ok(tx_requests)
    }

    /// Given an ordered set of Transaction Requests,
    /// Sign them and encode them for inclusion in a Bundle.
    pub async fn sign_and_encode_txns(
        &self,
        tx_requests: Vec<TransactionRequest>,
    ) -> Result<Vec<Bytes>, Error> {
        let mut encoded_txs: Vec<Bytes> = Vec::new();
        for mut tx in tx_requests {
            // fill out the transaction fields
            tx = tx
                .with_from(self.signer.address())
                .with_gas_limit(1_000_000)
                .with_max_priority_fee_per_gas((GWEI_TO_WEI * 16) as u128);

            // sign the transaction
            let SendableTx::Envelope(filled) = self.ru_provider.fill(tx).await? else {
                eyre::bail!("Failed to fill transaction")
            };

            // encode it
            let encoded = filled.encoded_2718();

            // add to array
            encoded_txs.push(Bytes::from(encoded));
        }
        Ok(encoded_txs)
    }
}

/// Example code demonstrating API usage and patterns for signing an Order.
#[derive(Debug)]
pub struct SendOrder<S: Signer> {
    /// The signer to use for signing the order.
    signer: S,
    /// The transaction cache endpoint.
    tx_cache: TxCache,
    /// The system constants.
    constants: SignetConstants,
}

impl<S> SendOrder<S>
where
    S: Signer,
{
    /// Create a new SendOrder instance.
    pub fn new(signer: S, constants: SignetConstants) -> Result<Self, Error> {
        Ok(Self {
            signer,
            tx_cache: TxCache::new_from_string(constants.environment().transaction_cache())?,
            constants,
        })
    }

    /// Construct a simple example Order, sign it, and send it.
    pub async fn run(&self) -> Result<(), Error> {
        // get an example order
        let order = self.example_order();

        // sign and send the order
        self.sign_and_send_order(order).await
    }

    /// Sign an Order and send it to the transaction cache to be Filled.
    pub async fn sign_and_send_order(&self, order: Order) -> Result<(), Error> {
        // make an UnsignedOrder from the Order
        let unsigned = UnsignedOrder::from(&order);

        // sign it
        let signed = unsigned
            .with_chain(self.constants.rollup().chain_id(), self.constants.rollup().orders())
            .sign(&self.signer)
            .await?;

        // send the SignedOrder to the transaction cache
        self.tx_cache.forward_order(signed).await
    }

    /// Get an example Order which swaps 1 USDC on the rollup for 1 USDC on the host.
    pub fn example_order(&self) -> Order {
        // input is 1 USDC on the rollup
        let input = Input { token: self.constants.rollup().tokens().usdc(), amount: ONE_USDC };

        // output is 1 USDC on the host chain
        let output = Output {
            token: self.constants.host().tokens().usdc(),
            amount: ONE_USDC,
            chainId: self.constants.host().chain_id() as u32,
            recipient: self.signer.address(),
        };

        // deadline 60 seconds (or ~5 blocks) from now
        let deadline = Utc::now().timestamp() + 60;

        // construct the order
        Order { inputs: vec![input], outputs: vec![output], deadline: U256::from(deadline) }
    }

    /// Check that the Order matches the example Order criteria.
    pub fn is_example_order(&self, order: &SignedOrder) -> bool {
        order.permit.permit.permitted.len() == 1
            && order.outputs.len() == 1
            && order.permit.permit.permitted[0].amount == ONE_USDC
            && order.outputs[0].amount == ONE_USDC
            && order.permit.permit.permitted[0].token == self.constants.rollup().tokens().usdc()
            && order.outputs[0].token == self.constants.host().tokens().usdc()
            && order.outputs[0].recipient == self.signer.address()
            && order.outputs[0].chainId == self.constants.host().chain_id() as u32
    }
}
