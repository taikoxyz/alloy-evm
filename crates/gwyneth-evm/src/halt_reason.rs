//! Halt reason surface for `alloy-gwyneth-evm`.
//!
//! The upstream REVM halt taxonomy (`revm::context_interface::result::HaltReason`) does not have a
//! structured representation for Gwyneth hard failures. To keep the hard-failure match signal
//! structured (and avoid stringly-typed checks), `alloy-gwyneth-evm` wraps the upstream halt
//! reason and adds a dedicated `GwynethHardFailure` variant.

use core::fmt;

use gwyneth_engine::GwynethHardFailure;
use revm::context_interface::result::HaltReason;

/// Halt reasons returned by the Gwyneth EVM wrapper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GwynethHaltReason {
    /// Standard REVM halt reason.
    Revm(HaltReason),
    /// Structured gwyneth hard-failure payload (normalized external fields).
    GwynethHardFailure(GwynethHardFailure),
}

impl From<HaltReason> for GwynethHaltReason {
    fn from(value: HaltReason) -> Self {
        Self::Revm(value)
    }
}

impl fmt::Display for GwynethHaltReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Revm(reason) => write!(f, "{reason:?}"),
            Self::GwynethHardFailure(reason) => fmt::Display::fmt(reason, f),
        }
    }
}
