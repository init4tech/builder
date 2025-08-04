use alloy::{
    consensus::constants::GWEI_TO_WEI,
    primitives::{Address, U256},
    signers::Signer,
};
use chrono::Utc;
use init4_bin_base::{
    deps::tracing::{debug, info, instrument},
    utils::{from_env::FromEnv, signer::LocalOrAws, tracing::init_tracing},
};
use orders::{
    filler::{Filler, FillerConfig},
    order::SendOrder,
    provider::{TxSenderProvider, connect_provider},
};
use signet_types::SignedOrder;
use signet_zenith::RollupOrders::{Input, Order, Output};
use tokio::time::{Duration, sleep};

#[derive(Debug, FromEnv)]
struct OrdersArgs {
    /// If present, the order will be filled on the rollup chain.
    /// If absent, the order will be filled on the host chain.
    #[from_env(
        var = "SEND_TO_ROLLUP",
        desc = "Whether to send the order to rollup or host. If true, it will be a RU-RU order. Else, it'll be a RU-HOST order."
    )]
    pub send_to_rollup: bool,
    #[from_env(var = "SLEEP_TIME", desc = "Time to sleep between transactions, in ms")]
    sleep_time: u64,
}

/// Construct, sign, and send a Signet Order, then Fill the same Order.
#[tokio::main]
async fn main() -> eyre::Result<()> {
    init_tracing();

    let config = FillerConfig::from_env()?;
    let OrdersArgs { send_to_rollup, sleep_time } = OrdersArgs::from_env()?;

    let signer = config.signer_config.connect().await?;
    let provider = connect_provider(signer.clone(), config.ru_rpc_url.clone()).await?;
    info!(signer_address = %signer.address(), "Connected to Signer and Provider");

    loop {
        let example_order = get_example_order(&config, signer.address(), send_to_rollup);

        let signed = send_order(example_order, &signer, &config).await?;
        debug!(?signed, "Order contents");
        info!("Order signed and sent to transaction cache");

        sleep(Duration::from_millis(500)).await;

        fill_orders(&signed, signer.clone(), provider.clone(), &config).await?;
        info!("Order filled successfully");

        sleep(Duration::from_millis(sleep_time)).await;
    }
}

/// Constructs an example [`Order`] based on the provided configuration and recipient address.
/// If `rollup` is true, it creates an order that targets the rollup; otherwise, it creates an order that targets the host chain.
fn get_example_order(config: &FillerConfig, recipient: Address, rollup: bool) -> Order {
    if rollup {
        Order {
            inputs: vec![Input {
                token: config.constants.rollup().tokens().weth(),
                amount: U256::from(GWEI_TO_WEI),
            }],
            outputs: vec![Output {
                token: config.constants.rollup().tokens().weth(),
                amount: U256::from(GWEI_TO_WEI),
                chainId: config.constants.rollup().chain_id() as u32,
                recipient,
            }],
            deadline: U256::from(Utc::now().timestamp() + (60 * 10)), // 10 minutes from now
        }
    } else {
        Order {
            inputs: vec![Input {
                token: config.constants.rollup().tokens().weth(),
                amount: U256::from(GWEI_TO_WEI),
            }],
            outputs: vec![Output {
                token: config.constants.host().tokens().weth(),
                amount: U256::from(GWEI_TO_WEI),
                chainId: config.constants.host().chain_id() as u32,
                recipient,
            }],
            deadline: U256::from(Utc::now().timestamp() + (60 * 10)), // 10 minutes from now
        }
    }
}

/// Sign and send an order to the transaction cache.
#[instrument(skip(order, signer, config), level = "debug", fields(signer_address = %signer.address()))]
async fn send_order(
    order: Order,
    signer: &LocalOrAws,
    config: &FillerConfig,
) -> eyre::Result<SignedOrder> {
    info!("signing and sending order");

    let send_order = SendOrder::new(signer.clone(), config.constants.clone())?;

    // sign the order, return it back for comparison
    let signed = send_order.sign_order(order).await?;

    // send the signed order to the transaction cache
    send_order.send_order(signed.clone()).await?;

    Ok(signed)
}

/// Fill example [`SignedOrder`]s from the transaction cache.
#[instrument(skip(target_order, signer, provider, config), level = "debug")]
async fn fill_orders(
    target_order: &SignedOrder,
    signer: LocalOrAws,
    provider: TxSenderProvider,
    config: &FillerConfig,
) -> eyre::Result<()> {
    info!("filling orders from transaction cache");
    let filler = Filler::new(signer, provider, config.constants.clone())?;

    // get all the [`SignedOrder`]s from tx cache
    let mut orders: Vec<SignedOrder> = filler.get_orders().await?;
    debug!(
        orders = ?orders,
        "Queried order contents from transaction cache"
    );

    // Retain only the orders that match the target order
    orders.retain(|o| o == target_order);

    // fill each individually
    filler.fill_individually(orders.as_slice()).await?;

    Ok(())
}
