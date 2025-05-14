//! This file implements the [`trevm::Cfg`] and [`trevm::Block`] traits for Pecorino blocks.
use trevm::revm::{context::CfgEnv, primitives::hardfork::SpecId};

/// PecorinoCfg holds network-level configuration values.
#[derive(Debug, Clone, Copy)]
pub struct SignetCfgEnv {
    /// The chain ID.
    pub chain_id: u64,
}

impl trevm::Cfg for SignetCfgEnv {
    /// Fills the configuration environment with Pecorino-specific values.
    ///
    /// # Arguments
    ///
    /// - `cfg_env`: The configuration environment to be filled.
    fn fill_cfg_env(&self, cfg_env: &mut CfgEnv) {
        let CfgEnv { chain_id, spec, .. } = cfg_env;

        *chain_id = self.chain_id;
        *spec = SpecId::default();
    }
}
