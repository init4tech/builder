//! This file implements the [`trevm::Cfg`] and [`trevm::Block`] traits for Signet and host networks.

use alloy_chains::NamedChain;
use alloy_hardforks::mainnet::MAINNET_OSAKA_TIMESTAMP;
use signet_constants::pecorino;
use trevm::revm::{context::CfgEnv, primitives::hardfork::SpecId};

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

    const fn spec_id(&self) -> SpecId {
        match self.chain_id {
            pecorino::HOST_CHAIN_ID | pecorino::RU_CHAIN_ID => SpecId::PRAGUE,
            id if id == NamedChain::Mainnet as u64 => self.mainnet_spec(),
            _ => SpecId::PRAGUE,
        }
    }

    const fn mainnet_spec(&self) -> SpecId {
        if self.timestamp >= MAINNET_OSAKA_TIMESTAMP { SpecId::OSAKA } else { SpecId::PRAGUE }
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

    #[test]
    fn unknown_chain_cfg_env() {
        let cfg = SignetCfgEnv::new(999999, 0);
        assert_eq!(cfg.spec_id(), SpecId::PRAGUE);
    }
}
