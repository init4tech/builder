use alloy::{primitives::Bytes, rpc::json_rpc::ErrorPayload, sol_types::SolError};
use signet_zenith::Zenith::{self, IncorrectHostBlock};

/// Represents the kind of revert that can occur during simulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SimRevertKind {
    /// Incorrect host block error
    IncorrectHostBlock,
    /// Bad signature error
    BadSignature,
    /// One rollup block per host block error
    OneRollupBlockPerHostBlock,
    /// Unknown error
    Unknown(Option<Bytes>),
}

impl From<Option<Bytes>> for SimRevertKind {
    fn from(data: Option<Bytes>) -> Self {
        let Some(data) = data else {
            return Self::Unknown(data);
        };

        if data.starts_with(&IncorrectHostBlock::SELECTOR) {
            Self::IncorrectHostBlock
        } else if data.starts_with(&Zenith::BadSignature::SELECTOR) {
            Self::BadSignature
        } else if data.starts_with(&Zenith::OneRollupBlockPerHostBlock::SELECTOR) {
            Self::OneRollupBlockPerHostBlock
        } else {
            Self::Unknown(Some(data))
        }
    }
}

#[derive(Debug, Clone)]
/// Represents an error that occurs during simulation of a transaction.
pub struct SimErrorResp {
    /// The error payload containing the error code and message.
    pub err: ErrorPayload,
    /// The kind of revert that occurred (or unknown if not recognized).
    kind: SimRevertKind,
}

impl core::fmt::Display for SimErrorResp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "SimErrorResp {{ code: {}, message: {}, kind: {:?} }}",
            self.code(),
            self.message(),
            self.kind
        )
    }
}

impl From<ErrorPayload> for SimErrorResp {
    fn from(err: ErrorPayload) -> Self {
        Self::new(err)
    }
}

impl SimErrorResp {
    /// Creates a new `SimRevertError` with the specified kind and error
    /// payload.
    pub fn new(err: ErrorPayload) -> Self {
        let kind = err.as_revert_data().into();
        Self { err, kind }
    }

    /// True if the error is an incorrect host block.
    pub fn is_incorrect_host_block(&self) -> bool {
        self.as_revert_data()
            .map(|b| b.starts_with(&IncorrectHostBlock::SELECTOR))
            .unwrap_or_default()
    }

    /// Attempts to decode the error payload as an [`IncorrectHostBlock`].
    pub fn as_incorrect_host_block(&self) -> Option<IncorrectHostBlock> {
        self.as_revert_data().and_then(|data| IncorrectHostBlock::abi_decode(&data).ok())
    }

    /// True if the error is a [`Zenith::BadSignature`].
    pub fn is_bad_signature(&self) -> bool {
        self.as_revert_data()
            .map(|b| b.starts_with(&Zenith::BadSignature::SELECTOR))
            .unwrap_or_default()
    }

    /// Attempts to decode the error payload as a [`Zenith::BadSignature`].
    pub fn as_bad_signature(&self) -> Option<Zenith::BadSignature> {
        self.as_revert_data().and_then(|data| Zenith::BadSignature::abi_decode(&data).ok())
    }

    /// True if the error is a [`Zenith::OneRollupBlockPerHostBlock`].
    pub fn is_one_rollup_block_per_host_block(&self) -> bool {
        self.as_revert_data()
            .map(|b| b.starts_with(&Zenith::OneRollupBlockPerHostBlock::SELECTOR))
            .unwrap_or_default()
    }

    /// Attempts to decode the error payload as a
    /// [`Zenith::OneRollupBlockPerHostBlock`].
    pub fn as_one_rollup_block_per_host_block(&self) -> Option<Zenith::OneRollupBlockPerHostBlock> {
        self.as_revert_data()
            .and_then(|data| Zenith::OneRollupBlockPerHostBlock::abi_decode(&data).ok())
    }

    /// True if the error is an unknown revert.
    pub fn is_unknown(&self) -> bool {
        !self.is_incorrect_host_block()
            && !self.is_bad_signature()
            && !self.is_one_rollup_block_per_host_block()
    }

    /// Returns the revert data if available.
    pub fn as_revert_data(&self) -> Option<Bytes> {
        self.err.as_revert_data()
    }

    /// Returns the JSON-RPC error code.
    pub const fn code(&self) -> i64 {
        self.err.code
    }

    /// Returns the error message.
    pub fn message(&self) -> &str {
        &self.err.message
    }
}
