//! Constants used in the builder.

use alloy::primitives::{Address, address};

/// The default basefee to use for simulation if RPC fails.
pub const BASEFEE_DEFAULT: u64 = 7;
/// Pecorino Host Chain ID used for the Pecorino network.
pub const PECORINO_HOST_CHAIN_ID: u64 = 3151908;
/// Pecorino Chain ID used for the Pecorino network.
pub const PECORINO_CHAIN_ID: u64 = 14174;
/// Block number at which the Pecorino rollup contract is deployed.
pub const PECORINO_DEPLOY_HEIGHT: u64 = 149984;
/// Address of the orders contract on the host.
pub const HOST_ORDERS: Address = address!("0x4E8cC181805aFC307C83298242271142b8e2f249");
/// Address of the passage contract on the host.
pub const HOST_PASSAGE: Address = address!("0xd553C4CA4792Af71F4B61231409eaB321c1Dd2Ce");
/// Address of the transactor contract on the host.
pub const HOST_TRANSACTOR: Address = address!("0x1af3A16857C28917Ab2C4c78Be099fF251669200");
/// Address of the USDC token contract on the host.
pub const HOST_USDC: Address = address!("0x885F8DB528dC8a38aA3DDad9D3F619746B4a6A81");
/// Address of the USDT token contract on the host.
pub const HOST_USDT: Address = address!("0x7970D259D4a96764Fa9B23FF0715A35f06f52D1A");
/// Address of the WBTC token contract on the host.
pub const HOST_WBTC: Address = address!("0x7970D259D4a96764Fa9B23FF0715A35f06f52D1A");
/// Address of the orders contract on the rollup.
pub const ROLLUP_ORDERS: Address = address!("0x4E8cC181805aFC307C83298242271142b8e2f249");
/// Address of the passage contract on the rollup.
pub const ROLLUP_PASSAGE: Address = address!("0xd553C4CA4792Af71F4B61231409eaB321c1Dd2Ce");
/// Base fee recipient address.
pub const BASE_FEE_RECIPIENT: Address = address!("0xe0eDA3701D44511ce419344A4CeD30B52c9Ba231");
/// Address of the USDC token contract on the rollup.
pub const ROLLUP_USDC: Address = address!("0x0B8BC5e60EE10957E0d1A0d95598fA63E65605e2");
/// Address of the USDT token contract on the rollup.
pub const ROLLUP_USDT: Address = address!("0xF34326d3521F1b07d1aa63729cB14A372f8A737C");
/// Address of the WBTC token contract on the rollup.
pub const ROLLUP_WBTC: Address = address!("0xE3d7066115f7d6b65F88Dff86288dB4756a7D733");
