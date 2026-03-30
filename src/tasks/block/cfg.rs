//! This file implements the [`trevm::Cfg`] and [`trevm::Block`] traits for Signet and host networks.
use alloy::eips::eip7840::BlobParams;
use reth_chainspec::{ChainSpec, EthChainSpec};
use signet_block_processor::revm_spec;
use signet_constants::{mainnet, parmigiana};
use signet_genesis::{
    MAINNET_GENESIS, MAINNET_HOST_GENESIS, PARMIGIANA_GENESIS, PARMIGIANA_HOST_GENESIS,
};
use std::sync::OnceLock;
use trevm::revm::{context::CfgEnv, primitives::hardfork::SpecId};

/// The RU [`ChainSpec`].
static RU_CHAINSPEC: OnceLock<ChainSpec> = OnceLock::new();

/// The Host [`ChainSpec`].
static HOST_CHAINSPEC: OnceLock<ChainSpec> = OnceLock::new();

/// [`SignetCfgEnv`] holds network-level configuration values.
#[derive(Debug, Clone, Copy)]
pub struct SignetCfgEnv {
    /// The chain ID.
    pub chain_id: u64,
    /// The block timestamp.
    pub timestamp: u64,
}

impl SignetCfgEnv {
    /// Creates a new [`SignetCfgEnv`].
    pub const fn new(chain_id: u64, timestamp: u64) -> Self {
        Self { chain_id, timestamp }
    }

    /// Returns a reference to the [`ChainSpec`] for the configured chain.
    fn chainspec(&self) -> &ChainSpec {
        match self.chain_id {
            parmigiana::RU_CHAIN_ID | mainnet::RU_CHAIN_ID => {
                RU_CHAINSPEC.get_or_init(|| initialize_ru_chainspec(self.chain_id))
            }
            parmigiana::HOST_CHAIN_ID | mainnet::HOST_CHAIN_ID => {
                HOST_CHAINSPEC.get_or_init(|| initialize_host_chainspec(self.chain_id))
            }
            _ => unimplemented!("Unknown chain ID: {}", self.chain_id),
        }
    }

    /// Returns the [`BlobParams`] for the configured chain and timestamp.
    ///
    /// Resolves the correct blob parameters from the chain spec's blob
    /// schedule, handling Cancun, Prague, Osaka, and BPO hardfork transitions.
    pub fn blob_params(&self) -> Option<BlobParams> {
        EthChainSpec::blob_params_at_timestamp(self.chainspec(), self.timestamp)
    }

    fn spec_id(&self) -> SpecId {
        revm_spec(self.chainspec(), self.timestamp)
    }
}

impl trevm::Cfg for SignetCfgEnv {
    fn fill_cfg_env(&self, cfg_env: &mut CfgEnv) {
        cfg_env.chain_id = self.chain_id;
        cfg_env.spec = self.spec_id();
    }
}

fn initialize_ru_chainspec(chain_id: u64) -> ChainSpec {
    match chain_id {
        parmigiana::RU_CHAIN_ID => ChainSpec::from_genesis(PARMIGIANA_GENESIS.to_owned()),
        mainnet::RU_CHAIN_ID => ChainSpec::from_genesis(MAINNET_GENESIS.to_owned()),
        _ => unimplemented!("Unknown rollup chain ID: {}", chain_id),
    }
}

fn initialize_host_chainspec(chain_id: u64) -> ChainSpec {
    match chain_id {
        parmigiana::HOST_CHAIN_ID => ChainSpec::from_genesis(PARMIGIANA_HOST_GENESIS.to_owned()),
        mainnet::HOST_CHAIN_ID => ChainSpec::from_genesis(MAINNET_HOST_GENESIS.to_owned()),
        _ => unimplemented!("Unknown host chain ID: {}", chain_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_chains::NamedChain;
    use alloy_hardforks::mainnet::MAINNET_OSAKA_TIMESTAMP;

    #[test]
    fn mainnet_cfg_env() {
        let cfg = SignetCfgEnv::new(NamedChain::Mainnet as u64, MAINNET_OSAKA_TIMESTAMP - 1);
        assert_eq!(cfg.spec_id(), SpecId::PRAGUE);

        let cfg = SignetCfgEnv::new(NamedChain::Mainnet as u64, MAINNET_OSAKA_TIMESTAMP);
        assert_eq!(cfg.spec_id(), SpecId::OSAKA);
    }
}
