//! This file implements the [`trevm::Cfg`] and [`trevm::Block`] traits for Pecorino blocks.
use alloy::primitives::{Address, B256, FixedBytes, U256};
use trevm::{
    Block,
    revm::{
        context::{BlockEnv, CfgEnv},
        context_interface::block::BlobExcessGasAndPrice,
        primitives::hardfork::SpecId,
    },
};

use crate::config::BuilderConfig;

/// PecorinoCfg holds network-level configuration values.
#[derive(Debug, Clone, Copy)]
pub struct PecorinoCfg {}

impl trevm::Cfg for PecorinoCfg {
    /// Fills the configuration environment with Pecorino-specific values.
    ///
    /// # Arguments
    ///
    /// - `cfg_env`: The configuration environment to be filled.
    fn fill_cfg_env(&self, cfg_env: &mut CfgEnv) {
        let CfgEnv { chain_id, spec, .. } = cfg_env;

        *chain_id = signet_constants::pecorino::RU_CHAIN_ID;
        *spec = SpecId::default();
    }
}

/// PecorinoBlockEnv holds block-level configurations for Pecorino blocks.
#[derive(Debug, Clone, Copy)]
pub struct PecorinoBlockEnv {
    /// The block number for this block.
    pub number: u64,
    /// The address the block reward should be sent to.
    pub beneficiary: Address,
    /// Timestamp for the block.
    pub timestamp: u64,
    /// The gas limit for this block environment.
    pub gas_limit: u64,
    /// The basefee to use for calculating gas usage.
    pub basefee: u64,
    /// The prevrandao to use for this block.
    pub prevrandao: Option<FixedBytes<32>>,
}

/// Implements [`trevm::Block`] for the Pecorino block.
impl Block for PecorinoBlockEnv {
    /// Fills the block environment with the Pecorino specific values
    fn fill_block_env(&self, block_env: &mut BlockEnv) {
        // Destructure the fields off of the block_env and modify them
        let BlockEnv {
            number,
            beneficiary,
            timestamp,
            gas_limit,
            basefee,
            difficulty,
            prevrandao,
            blob_excess_gas_and_price,
        } = block_env;
        *number = self.number;
        *beneficiary = self.beneficiary;
        *timestamp = self.timestamp;
        *gas_limit = self.gas_limit;
        *basefee = self.basefee;
        *prevrandao = self.prevrandao;

        // NB: The following fields are set to sane defaults because they
        // are not supported by the rollup
        *difficulty = U256::ZERO;
        *blob_excess_gas_and_price =
            Some(BlobExcessGasAndPrice { excess_blob_gas: 0, blob_gasprice: 0 });
    }
}

impl PecorinoBlockEnv {
    /// Returns a new PecorinoBlockEnv with the specified values.
    ///
    /// # Arguments
    ///
    /// - config: The BuilderConfig for the builder.
    /// - number: The block number of this block, usually the latest block number plus 1,
    ///   unless simulating blocks in the past.
    /// - timestamp: The timestamp of the block, typically set to the deadline of the
    ///   block building task.
    pub fn new(config: BuilderConfig, number: u64, timestamp: u64, basefee: u64) -> Self {
        PecorinoBlockEnv {
            number,
            beneficiary: config.builder_rewards_address,
            timestamp,
            gas_limit: config.rollup_block_gas_limit,
            basefee,
            prevrandao: Some(B256::random()),
        }
    }
}
