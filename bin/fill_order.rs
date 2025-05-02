use std::{borrow::Cow, ops::Add, str::FromStr};
use builder::config::{load_string, connect_provider};
use builder::signer::LocalOrAws;
use init4_bin_base::from_env::FromEnv;
use signet::rpc::TxCache;
use signet::types::{AggregateOrders, SignedFill, SignedOrder, UnsignedFill};
use signet::zenith::RollupOrders::Order;
use signet::bundle::SignetEthBundle;
use std::collections::HashMap;
use eyre::{eyre, Error};
use alloy::{
    eips::Encodable2718,
    network::{Ethereum, EthereumWallet, TransactionBuilder},
    primitives::{Address, Bytes},
    providers::RootProvider,
    rpc::types::{mev::EthSendBundle, TransactionRequest},
    signers::{Signer, local::PrivateKeySigner},
};

/// Multiplier for converting gwei to wei.
const GWEI_TO_WEI: u64 = 1_000_000_000;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // get signer pk
    let signer_key = load_string("SIGNER_KEY").unwrap();

    // instantiate pecorino config with signer pk
    let config = FillerConfig::pecorino(signer_key);

    // instantiate Filler
    let filler = Filler::from(config);

    // GET all orders from tx cache
    let orders: Vec<SignedOrder> = filler.get_orders().await?;

    // filter only 1:1 USDC swaps
    let fillable_orders = orders
        .iter()
        .filter(|signed_order| is_one_to_one_usdc_swap(signed_order, &config) && is_from_self(signed_order, &filler))
        .collect();

    // fill each order individually
    filler.fill_individually(fillable_orders).await
}

/// Returns true if the Order is a 1:1 USDC swap from rollup to host
const fn is_one_to_one_usdc_swap(signed_order: SignedOrder, config: &FillerConfig) -> bool {
    let order = Order::from(signed_order);

    let one_input = order.inputs.len() == 1;
    let one_output = order.outputs.len() == 1;

    let input_is_rollup_usdc = order.inputs[0].token == config.rollup_usdc_address;
    let output_is_host_usdc = order.outputs[0].chainId == config.host_chain_id
        && order.outputs[0].token == config.host_usdc_address;

    let input_and_output_amounts_equal = order.inputs[0].amount == order.outputs[0].amount;

    one_input
        && one_output
        && input_is_rollup_usdc
        && output_is_host_usdc
        && input_and_output_amounts_equal
}

/// Returns true if the Order was initiated by the Filler themselves
const fn is_from_self(signed_order: SignedOrder, filler: &Filler) -> bool {
    signed_order.permit.owner == filler.signer.address()
}

/// Configuration values necessary to instantiate a Filler.
struct FillerConfig {
    /// The private key of the signer to use for signing transactions.
    #[from_env(var = "SIGNER_KEY")]
    signer_key: String,
    /// The RPC URL of the rollup.
    #[from_env(var = "ROLLUP_RPC")]
    rollup_rpc: String,
    /// The endpoint for the transaction cache.
    #[from_env(var = "TX_CACHE_ENDPOINT")]
    tx_cache_endpoint: String,
    /// The chain ID of the rollup.
    #[from_env(var = "RU_CHAIN_ID")]
    ru_chain_id: u64,
    /// The chain ID of the host.
    #[from_env(var = "HOST_CHAIN_ID")]
    host_chain_id: u64,
    /// The address of the order contract on the host chain.
    #[from_env(var = "HOST_ORDER_CONTRACT")]
    host_order_contract: Address,
    /// The address of the order contract on the rollup chain.
    #[from_env(var = "ROLLUP_ORDER_CONTRACT")]
    rollup_order_contract: Address,
    /// The address of USDC on the rollup.
    #[from_env(var = "ROLLUP_USDC_ADDRESS")]
    rollup_usdc_address: Address,
    /// The address of USDC on the host chain.
    #[from_env(var = "HOST_USDC_ADDRESS")]
    host_usdc_address: Address,
}

impl FillerConfig {
    /// Load the configuration for Pecorino.
    fn pecorino(signer_key: String) -> Self {
        Self {
            signer_key,
            rollup_rpc: "https://rpc.pecorino.signet.sh".to_string(),
            tx_cache_endpoint: "https://transactions.pecorino.signet.sh".to_string(),
            ru_chain_id: 14174,
            host_chain_id: 3151908,
            host_order_contract: Address::from_str(
                &"0x4E8cC181805aFC307C83298242271142b8e2f249".to_string(),
            )
            .unwrap(),
            rollup_order_contract: Address::from_str(
                &"0x4E8cC181805aFC307C83298242271142b8e2f249".to_string(),
            )
            .unwrap(),
            host_usdc_address: Address::from_str(
                &"0x885F8DB528dC8a38aA3DDad9D3F619746B4a6A81".to_string(),
            )
            .unwrap(),
            rollup_usdc_address: Address::from_str(
                &"0x0B8BC5e60EE10957E0d1A0d95598fA63E65605e2".to_string(),
            )
            .unwrap(),
        }
    }
}

impl From<FillerConfig> for Filler {
    async fn from(config: FillerConfig) -> Result<Self, eyre::Error> {
        // instantiate signer
        let signer = LocalOrAws::load(&config.signer_key, Some(self.host_chain_id)).await.map_err(Into::into)?;
        // instantiate provider
        let provider = connect_provider(&config.rollup_rpc).await?;
        // instantiate tx cache
        let tx_cache = TxCache::new(&config.tx_cache_endpoint);
        // add host & rollup to order_contracts mapping
        let mut order_contracts = HashMap::new();
        order_contracts.insert(config.ru_chain_id, config.rollup_order_contract);
        order_contracts.insert(config.host_chain_id, config.host_order_contract);
        // return Filler instance
        Ok(Self::new(
            signer,
            provider,
            tx_cache,
            order_contracts,
            config.ru_chain_id,
            config.host_chain_id,
        ))
    }
}

/// Example code demonstrating API usage and patterns for Signet Fillers.
#[derive(Debug)]
struct Filler<S: Signer> {
    /// The signer to use for signing transactions.
    signer: S,
    /// The provider to use for building transactions on the Rollup.
    ru_provider: RootProvider<Ethereum>,
    /// The transaction cache endpoint.
    tx_cache: TxCache,
    /// A HashMap of the Order contract addresses for each chain.
    /// MUST contain an address for both Host and Rollup.
    order_contracts: HashMap<u64, Address>,
    /// The chain id of the rollup.
    ru_chain_id: u64,
    /// The chain id of the host.
    host_chain_id: u64,
}

impl<S> Filler<S>
where
    S: Signer,
{
    /// Create a new Filler with the given signer, provider, and transaction cache endpoint.
    pub const fn new(
        signer: S,
        ru_provider: RootProvider<Ethereum>,
        tx_cache: TxCache,
        order_contracts: HashMap<u64, Address>,
        ru_chain_id: u64,
        host_chain_id: u64,
    ) -> Self {
        Self { signer, ru_provider, tx_cache, order_contracts, ru_chain_id, host_chain_id }
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
    pub async fn fill_individually(&self, orders:Vec<SignedOrder>) -> Result<(), Error> {
        // submit one bundle per individual order
        for order in orders {
            self.fill(vec![order]).await?;
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
    pub async fn fill(&self, orders: Vec<SignedOrder>) -> Result<(), Error> {
        // if orders is empty, exit the function without doing anything
        if orders.is_empty() {
            println!("No orders to fill");
            return Ok(());
        }

        // sign a SignedFill for the orders
        let signed_fills = self.sign_fills(orders.clone()).await?;

        // get the transaction requests for the rollup
        let tx_requests = self.rollup_txn_requests(&signed_fills, &orders).await?;

        // sign & encode the transactions for the Bundle
        let txs = self.sign_and_encode_txns(tx_requests).await?;

        // get the aggregated host fill for the Bundle, if any
        let host_fills = signed_fills.get(&self.host_chain_id).cloned();

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
    pub async fn sign_fills(
        &self,
        orders: Vec<SignedOrder>,
    ) -> Result<HashMap<u64, SignedFill>, Error> {
        //  create an AggregateOrder from the SignedOrders they want to fill
        let agg = AggregateOrders::from(orders);
        // produce an UnsignedFill from the AggregateOrder
        let mut unsigned_fill = UnsignedFill::from(&agg);
        // populate the Order contract addresses for each chain
        for chain_id in agg.destination_chain_ids() {
            unsigned_fill =
                unsigned_fill.with_chain(chain_id, self.order_contract_address_for(chain_id)?);
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
        orders: &Vec<SignedOrder>,
    ) -> Result<Vec<TransactionRequest>, Error> {
        // construct the transactions to be submitted to the Rollup
        let mut tx_requests = Vec::new();

        // first, if there is a SignedFill for the Rollup, add a transaction to submit the fill
        // Note that `fill` transactions MUST be mined *before* the corresponding Order(s) `initiate` transactions in order to cound
        // Host `fill` transactions are always considered to be mined "before" the rollup block is processed,
        // but Rollup `fill` transactions MUST take care to be ordered before the Orders are `initiate`d
        if let Some(rollup_fill) = signed_fills.get(&self.ru_chain_id) {
            // add the fill tx to the rollup txns
            let ru_fill_tx = rollup_fill.to_fill_tx(self.ru_order_contract()?);
            tx_requests.push(ru_fill_tx);
        }

        // next, add a transaction to initiate each SignedOrder
        for signed_order in orders {
            // add the initiate tx to the rollup txns
            let ru_initiate_tx =
                signed_order.to_initiate_tx(self.signer.address(), self.ru_order_contract()?);
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
                return Err(eyre!("Failed to fill rollup transaction"));
            };

            // encode it
            let encoded = filled.encoded_2718();

            // add to array
            encoded_txs.push(Bytes::from(encoded));
        }
        Ok(encoded_txs)
    }

    /// Get the Order contract address for the given chain id.
    fn order_contract_address_for(&self, chain_id: u64) -> Result<Address, Error> {
        self.order_contracts
            .get(&chain_id)
            .cloned()
            .ok_or(eyre!("No Order contract address configured for chain id {}", chain_id))
    }

    /// Get the Order contract address for the rollup.
    fn ru_order_contract(&self) -> Result<Address, Error> {
        self.order_contract_address_for(self.ru_chain_id)
    }
}
