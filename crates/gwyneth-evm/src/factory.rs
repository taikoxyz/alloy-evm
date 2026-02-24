//! Gwyneth EVM factory trait.
//!
//! This module provides [`GwynethEvmFactory`], a trait that mirrors [`alloy_evm::EvmFactory`]
//! but with the additional [`ChainSwitchable`] bound on the database type.
//!
//! This separation is necessary because Gwyneth's multi-chain execution requires databases
//! that can switch between chain contexts, which is not expressible through the standard
//! `EvmFactory` trait's generic bounds.
//!
//! [`ChainSwitchable`]: gwyneth_types::ChainSwitchable

use crate::{GwynethEvm, GwynethEvmContext};
use alloy_evm::{Database, EvmEnv};
use core::{error::Error, fmt::Debug, hash::Hash};
use gwyneth_detector::DetectorConfig;
use gwyneth_types::ChainSwitchable;
use revm::{
    context_interface::result::HaltReasonTr,
    inspector::{Inspector, NoOpInspector},
    primitives::hardfork::SpecId,
};

/// A factory for creating Gwyneth EVM instances.
///
/// This trait mirrors [`alloy_evm::EvmFactory`] but adds the [`ChainSwitchable`] bound
/// to support multi-chain execution. It uses Generic Associated Types (GATs) to be
/// flexible over the database and inspector types.
///
/// # Example
///
/// ```ignore
/// use alloy_gwyneth_evm::factory::{GwynethEvmFactory, GwynethEvmFactoryImpl};
/// use gwyneth_engine::L2OverlayDb;
///
/// let factory = GwynethEvmFactoryImpl::default();
/// let evm = factory.create_gwyneth_evm(db, env);
/// ```
pub trait GwynethEvmFactory {
    /// The EVM type that this factory creates.
    type Evm<DB: Database + ChainSwitchable, I: Inspector<GwynethEvmContext<DB>>>: alloy_evm::Evm<
        DB = DB,
        Tx = Self::Tx,
        HaltReason = Self::HaltReason,
        Error = Self::Error<DB::Error>,
        Spec = Self::Spec,
        Precompiles = Self::Precompiles,
        Inspector = I,
    >;

    /// The EVM context for inspectors.
    type Context<DB: Database + ChainSwitchable>: revm::context_interface::ContextTr<
        Db = DB,
        Journal: revm::inspector::JournalExt,
    >;

    /// Transaction environment.
    type Tx: alloy_evm::IntoTxEnv<Self::Tx>;

    /// EVM error type.
    type Error<DBError: Error + Send + Sync + 'static>: alloy_evm::EvmError;

    /// Halt reason.
    type HaltReason: HaltReasonTr + Send + Sync + 'static;

    /// The EVM specification identifier.
    type Spec: Debug + Copy + Hash + Eq + Send + Sync + Default + 'static;

    /// Precompiles used by the EVM.
    type Precompiles;

    /// Creates a new Gwyneth EVM instance.
    fn create_gwyneth_evm<DB: Database + ChainSwitchable>(
        &self,
        db: DB,
        evm_env: EvmEnv<Self::Spec>,
    ) -> Self::Evm<DB, NoOpInspector>;

    /// Creates a new Gwyneth EVM instance with an inspector.
    fn create_gwyneth_evm_with_inspector<
        DB: Database + ChainSwitchable,
        I: Inspector<GwynethEvmContext<DB>>,
    >(
        &self,
        db: DB,
        env: EvmEnv<Self::Spec>,
        inspector: I,
    ) -> Self::Evm<DB, I>;
}

/// Implementation of [`GwynethEvmFactory`] that creates [`GwynethEvm`] instances.
///
/// This is the primary factory implementation for Gwyneth EVMs. It holds a
/// [`DetectorConfig`] that configures cross-chain call detection behavior.
#[derive(Debug, Clone, Default)]
pub struct GwynethEvmFactoryImpl {
    /// Configuration for the Gwyneth detector.
    pub detector_config: DetectorConfig,
}

impl GwynethEvmFactoryImpl {
    /// Create a new factory with default detector configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new factory with custom detector configuration.
    pub const fn with_detector_config(detector_config: DetectorConfig) -> Self {
        Self { detector_config }
    }

    /// Get a reference to the detector configuration.
    pub const fn detector_config(&self) -> &DetectorConfig {
        &self.detector_config
    }
}

impl GwynethEvmFactory for GwynethEvmFactoryImpl {
    type Evm<DB: Database + ChainSwitchable, I: Inspector<GwynethEvmContext<DB>>> =
        GwynethEvm<DB, I>;
    type Context<DB: Database + ChainSwitchable> = GwynethEvmContext<DB>;
    type Tx = revm::context::TxEnv;
    type Error<DBError: Error + Send + Sync + 'static> = revm::context_interface::result::EVMError<
        DBError,
        revm::context_interface::result::InvalidTransaction,
    >;
    type HaltReason = revm::context_interface::result::HaltReason;
    type Spec = SpecId;
    type Precompiles = gwyneth_engine::GwynethPrecompileProvider;

    fn create_gwyneth_evm<DB: Database + ChainSwitchable>(
        &self,
        db: DB,
        evm_env: EvmEnv<Self::Spec>,
    ) -> Self::Evm<DB, NoOpInspector> {
        GwynethEvm::from_env(db, evm_env, NoOpInspector, self.detector_config.clone(), false)
    }

    fn create_gwyneth_evm_with_inspector<
        DB: Database + ChainSwitchable,
        I: Inspector<GwynethEvmContext<DB>>,
    >(
        &self,
        db: DB,
        env: EvmEnv<Self::Spec>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        GwynethEvm::from_env(db, env, inspector, self.detector_config.clone(), true)
    }
}
