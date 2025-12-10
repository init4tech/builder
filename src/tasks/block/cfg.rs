//! This file implements the [`trevm::Cfg`] and [`trevm::Block`] traits for Signet and host networks.
use reth_chainspec::ChainSpec;
use signet_block_processor::revm_spec;
use signet_constants::{mainnet, pecorino};
use signet_genesis::{
    MAINNET_GENESIS, MAINNET_HOST_GENESIS, PECORINO_GENESIS, PECORINO_HOST_GENESIS,
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

    fn spec_id(&self) -> SpecId {
        match self.chain_id {
            pecorino::RU_CHAIN_ID | mainnet::RU_CHAIN_ID => revm_spec(
                RU_CHAINSPEC.get_or_init(|| initialize_ru_chainspec(self.chain_id)),
                self.timestamp,
            ),
            pecorino::HOST_CHAIN_ID | mainnet::HOST_CHAIN_ID => revm_spec(
                HOST_CHAINSPEC.get_or_init(|| initialize_host_chainspec(self.chain_id)),
                self.timestamp,
            ),
            _ => unimplemented!("Unknown chain ID: {}", self.chain_id),
        }
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
        pecorino::RU_CHAIN_ID => ChainSpec::from_genesis(PECORINO_GENESIS.to_owned()),
        mainnet::RU_CHAIN_ID => ChainSpec::from_genesis(MAINNET_GENESIS.to_owned()),
        _ => unimplemented!("Unknown rollup chain ID: {}", chain_id),
    }
}

fn initialize_host_chainspec(chain_id: u64) -> ChainSpec {
    match chain_id {
        pecorino::HOST_CHAIN_ID => ChainSpec::from_genesis(PECORINO_HOST_GENESIS.to_owned()),
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
