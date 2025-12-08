//! This file implements the [`trevm::Cfg`] and [`trevm::Block`] traits for Signet and host networks.
use reth_chainspec::ChainSpec;
use signet_block_processor::revm_spec;
use signet_constants::{mainnet, pecorino};
use signet_genesis::PECORINO_GENESIS;
use std::sync::LazyLock;
use trevm::revm::{context::CfgEnv, primitives::hardfork::SpecId};

/// The RU Pecorino [`ChainSpec`].
static PECORINO_SPEC: LazyLock<ChainSpec> =
    LazyLock::new(|| ChainSpec::from_genesis(PECORINO_GENESIS.to_owned()));

/// The RU Mainnet [`ChainSpec`].
static MAINNET_RU_SPEC: LazyLock<ChainSpec> =
    LazyLock::new(|| ChainSpec::from_genesis(signet_genesis::MAINNET_GENESIS.to_owned()));

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
            // Pecorino
            pecorino::HOST_CHAIN_ID | pecorino::RU_CHAIN_ID => {
                revm_spec(&PECORINO_SPEC, self.timestamp)
            }
            // Mainnet RU
            mainnet::RU_CHAIN_ID => revm_spec(&MAINNET_RU_SPEC, self.timestamp),
            mainnet::HOST_CHAIN_ID => revm_spec(&reth_chainspec::MAINNET, self.timestamp),
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_chains::NamedChain;
    use alloy_hardforks::mainnet::MAINNET_OSAKA_TIMESTAMP;

    #[test]
    fn pecorino_cfg_env() {
        let cfg = SignetCfgEnv::new(pecorino::HOST_CHAIN_ID, 0);
        assert_eq!(cfg.spec_id(), SpecId::PRAGUE);

        let cfg = SignetCfgEnv::new(pecorino::RU_CHAIN_ID, 0);
        assert_eq!(cfg.spec_id(), SpecId::PRAGUE);
    }

    #[test]
    fn mainnet_cfg_env() {
        let cfg = SignetCfgEnv::new(NamedChain::Mainnet as u64, MAINNET_OSAKA_TIMESTAMP - 1);
        assert_eq!(cfg.spec_id(), SpecId::PRAGUE);

        let cfg = SignetCfgEnv::new(NamedChain::Mainnet as u64, MAINNET_OSAKA_TIMESTAMP);
        assert_eq!(cfg.spec_id(), SpecId::OSAKA);
    }
}
