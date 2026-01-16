//! Gwyneth EVM factory wiring.
//!
//! Gwyneth's multi-chain execution requires additional DB capabilities (`ChainSwitchable`,
//! checkpoint loaders, etc). These bounds are expressed via the canonical
//! [`alloy_evm::evm::BoundedEvmFactory`] adapter trait on a thin wrapper returned by
//! [`GwynethEvmFactoryImpl::for_surface`].

use crate::{GwynethEvmContext, GwynethRunner};
use alloy_evm::evm::BoundedEvmFactory;
use alloy_evm::{Database, EvmEnv};
use core::error::Error;
use gwyneth_detector::DetectorConfig;
use gwyneth_types::{ChainSwitchable, ExecutionSurface, ParentLoadCheckpoints};
use revm::{
    inspector::{Inspector, NoOpInspector},
    primitives::hardfork::SpecId,
};

/// Factory for creating [`GwynethRunner`] instances.
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
