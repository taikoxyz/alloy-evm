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

    use crate::{GwynethEvmContext, GwynethRunner};
    use alloy_evm::{Database, EvmEnv};
    use alloy_evm::evm::BoundedEvmFactory;
use core::{error::Error, fmt::Debug, hash::Hash};
use gwyneth_detector::DetectorConfig;
use gwyneth_types::{ChainSwitchable, ExecutionSurface, ParentLoadCheckpoints};
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
/// use gwyneth_types::ExecutionSurface;
///
/// let factory = GwynethEvmFactoryImpl::default();
/// let evm = factory.create_gwyneth_evm(db, env, ExecutionSurface::TxSubmission);
/// ```
pub trait GwynethEvmFactory {
    /// The EVM type that this factory creates.
    type Evm<
        DB: Database + ChainSwitchable + ParentLoadCheckpoints,
        I: Inspector<GwynethEvmContext<DB>>,
    >: alloy_evm::Evm<
        DB = DB,
        Tx = Self::Tx,
        HaltReason = Self::HaltReason,
        Error = Self::Error<<DB as revm::Database>::Error>,
        Spec = Self::Spec,
        Precompiles = Self::Precompiles,
        Inspector = crate::GwynethInspector<I>,
    >;

    /// The EVM context for inspectors.
    type Context<DB: Database + ChainSwitchable + ParentLoadCheckpoints>:
        revm::context_interface::ContextTr<
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
    fn create_gwyneth_evm<DB: Database + ChainSwitchable + ParentLoadCheckpoints>(
        &self,
        db: DB,
        evm_env: EvmEnv<Self::Spec>,
        surface: ExecutionSurface,
    ) -> Self::Evm<DB, NoOpInspector>;

    /// Creates a new Gwyneth EVM instance with an inspector.
    fn create_gwyneth_evm_with_inspector<
        DB: Database + ChainSwitchable + ParentLoadCheckpoints,
        I: Inspector<GwynethEvmContext<DB>>,
    >(
        &self,
        db: DB,
        env: EvmEnv<Self::Spec>,
        surface: ExecutionSurface,
        inspector: I,
    ) -> Self::Evm<DB, I>;
}

/// Implementation of [`GwynethEvmFactory`] that creates [`GwynethRunner`] instances.
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
    pub fn with_detector_config(detector_config: DetectorConfig) -> Self {
        Self { detector_config }
    }

    /// Get a reference to the detector configuration.
    pub fn detector_config(&self) -> &DetectorConfig {
        &self.detector_config
    }
}

impl GwynethEvmFactory for GwynethEvmFactoryImpl {
    type Evm<DB: Database + ChainSwitchable + ParentLoadCheckpoints, I: Inspector<GwynethEvmContext<DB>>> =
        GwynethRunner<DB, I>;
    type Context<DB: Database + ChainSwitchable + ParentLoadCheckpoints> = GwynethEvmContext<DB>;
    type Tx = revm::context::TxEnv;
    type Error<DBError: Error + Send + Sync + 'static> =
        revm::context_interface::result::EVMError<DBError, revm::context_interface::result::InvalidTransaction>;
    type HaltReason = crate::GwynethHaltReason;
    type Spec = SpecId;
    type Precompiles = gwyneth_engine::GwynethPrecompileProvider;

    fn create_gwyneth_evm<DB: Database + ChainSwitchable + ParentLoadCheckpoints>(
        &self,
        db: DB,
        evm_env: EvmEnv<Self::Spec>,
        surface: ExecutionSurface,
    ) -> Self::Evm<DB, NoOpInspector> {
        self.for_surface(surface).create_evm(db, evm_env)
    }

    fn create_gwyneth_evm_with_inspector<
        DB: Database + ChainSwitchable + ParentLoadCheckpoints,
        I: Inspector<GwynethEvmContext<DB>>,
    >(
        &self,
        db: DB,
        env: EvmEnv<Self::Spec>,
        surface: ExecutionSurface,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        self.for_surface(surface)
            .create_evm_with_inspector(db, env, inspector)
    }
}

/// A thin wrapper around [`GwynethEvmFactoryImpl`] that fixes the [`ExecutionSurface`].
#[derive(Debug, Clone, Copy)]
pub struct GwynethSurfaceFactory<'a> {
    inner: &'a GwynethEvmFactoryImpl,
    surface: ExecutionSurface,
}

impl GwynethEvmFactoryImpl {
    /// Returns an adapter that creates EVMs for the given execution surface.
    pub fn for_surface(&self, surface: ExecutionSurface) -> GwynethSurfaceFactory<'_> {
        GwynethSurfaceFactory { inner: self, surface }
    }
}

impl<'a, DB> BoundedEvmFactory<DB> for GwynethSurfaceFactory<'a>
where
    DB: Database + ChainSwitchable + ParentLoadCheckpoints,
{
    type Evm<I: Inspector<Self::Context>> = GwynethRunner<DB, I>;
    type Context = GwynethEvmContext<DB>;
    type EvmInspector<I: Inspector<Self::Context>> = crate::GwynethInspector<I>;
    type Tx = revm::context::TxEnv;
    type Error<DBError: Error + Send + Sync + 'static> =
        revm::context_interface::result::EVMError<DBError, revm::context_interface::result::InvalidTransaction>;
    type HaltReason = crate::GwynethHaltReason;
    type Spec = SpecId;
    type BlockEnv = revm::context::BlockEnv;
    type Precompiles = gwyneth_engine::GwynethPrecompileProvider;

    fn create_evm(
        &self,
        db: DB,
        evm_env: EvmEnv<Self::Spec, Self::BlockEnv>,
    ) -> Self::Evm<NoOpInspector> {
        self.create_evm_with_inspector(db, evm_env, NoOpInspector)
    }

    fn create_evm_with_inspector<I: Inspector<Self::Context>>(
        &self,
        db: DB,
        input: EvmEnv<Self::Spec, Self::BlockEnv>,
        inspector: I,
    ) -> Self::Evm<I> {
        GwynethRunner::from_env(
            db,
            input,
            inspector,
            self.inner.detector_config.clone(),
            self.surface,
            true,
        )
    }
}
