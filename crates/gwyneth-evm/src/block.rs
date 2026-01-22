//! Lightweight block-execution scaffolding for Gwyneth.
//!
//! This module provides block execution types that work with Gwyneth's multi-chain EVM.
//! It mirrors `alloy_evm::block` but uses [`GwynethEvmFactory`] which requires `ChainSwitchable`.
//!
//! [`GwynethEvmFactory`]: crate::GwynethEvmFactory

extern crate alloc;

use crate::{factory::GwynethEvmFactory, GwynethEvmContext};
use alloc::borrow::Cow;
use alloy_consensus::Header;
use alloy_eips::eip4895::Withdrawals;
use alloy_evm::{eth::EthBlockExecutionCtx, Database, EvmEnv};
use alloy_primitives::B256;
use gwyneth_types::{ChainSwitchable, ParentLoadCheckpoints};
use revm::{
    inspector::{Inspector, NoOpInspector},
    primitives::hardfork::SpecId,
};

/// Execution context for Gwyneth block execution.
///
/// This provides block-level data needed during execution, such as the parent hash,
/// beacon block root, ommers, and withdrawals for system calls and block finalization.
/// It is compatible with [`EthBlockExecutionCtx`] and can be converted to/from it.
#[derive(Debug, Clone)]
pub struct GwynethBlockExecutionCtx<'a> {
    /// Parent block hash for EIP-2935 blockhash system call.
    pub parent_hash: B256,
    /// Parent beacon block root for EIP-4788 beacon root system call.
    pub parent_beacon_block_root: Option<B256>,
    /// Ommers (uncle blocks) - empty for post-merge blocks.
    pub ommers: &'a [Header],
    /// Withdrawals for the block (EIP-4895).
    pub withdrawals: Option<Cow<'a, Withdrawals>>,
}

impl Default for GwynethBlockExecutionCtx<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> GwynethBlockExecutionCtx<'a> {
    /// Create a new execution context with default values.
    pub fn new() -> Self {
        Self {
            parent_hash: B256::ZERO,
            parent_beacon_block_root: None,
            ommers: &[],
            withdrawals: None,
        }
    }

    /// Create a context with all fields specified.
    pub fn with_all(
        parent_hash: B256,
        parent_beacon_block_root: Option<B256>,
        ommers: &'a [Header],
        withdrawals: Option<Cow<'a, Withdrawals>>,
    ) -> Self {
        Self { parent_hash, parent_beacon_block_root, ommers, withdrawals }
    }

    /// Create a context with parent hash and beacon root, with empty ommers and no withdrawals.
    /// Suitable for post-merge blocks where ommers are always empty.
    pub fn with_parent_hash(parent_hash: B256, parent_beacon_block_root: Option<B256>) -> Self {
        Self { parent_hash, parent_beacon_block_root, ommers: &[], withdrawals: None }
    }
}

impl<'a> From<EthBlockExecutionCtx<'a>> for GwynethBlockExecutionCtx<'a> {
    fn from(ctx: EthBlockExecutionCtx<'a>) -> Self {
        Self {
            parent_hash: ctx.parent_hash,
            parent_beacon_block_root: ctx.parent_beacon_block_root,
            ommers: ctx.ommers,
            withdrawals: ctx.withdrawals,
        }
    }
}

impl<'a> From<GwynethBlockExecutionCtx<'a>> for EthBlockExecutionCtx<'a> {
    fn from(ctx: GwynethBlockExecutionCtx<'a>) -> Self {
        EthBlockExecutionCtx {
            parent_hash: ctx.parent_hash,
            parent_beacon_block_root: ctx.parent_beacon_block_root,
            ommers: ctx.ommers,
            withdrawals: ctx.withdrawals,
        }
    }
}

/// A thin wrapper around a Gwyneth EVM instance for block execution flows.
#[derive(Debug)]
pub struct GwynethBlockExecutor<'a, Evm> {
    evm: Evm,
    ctx: GwynethBlockExecutionCtx<'a>,
}

impl<'a, Evm> GwynethBlockExecutor<'a, Evm> {
    /// Construct a new executor given a configured EVM and execution context.
    pub fn new(evm: Evm, ctx: GwynethBlockExecutionCtx<'a>) -> Self {
        Self { evm, ctx }
    }

    /// Destructure into the underlying parts.
    pub fn into_parts(self) -> (Evm, GwynethBlockExecutionCtx<'a>) {
        (self.evm, self.ctx)
    }

    /// Access the inner EVM.
    pub fn evm_mut(&mut self) -> &mut Evm {
        &mut self.evm
    }

    /// Access the inner EVM immutably.
    pub fn evm(&self) -> &Evm {
        &self.evm
    }

    /// Access the execution context.
    pub fn ctx(&self) -> &GwynethBlockExecutionCtx<'a> {
        &self.ctx
    }
}

/// Trait for factories that create Gwyneth block executors.
///
/// This mirrors [`alloy_evm::block::BlockExecutorFactory`] but uses [`GwynethEvmFactory`]
/// which requires the `ChainSwitchable` bound on databases.
#[auto_impl::auto_impl(Arc)]
pub trait GwynethBlockExecutorFactoryTrait: 'static {
    /// The EVM factory used by this executor factory.
    type EvmFactory: GwynethEvmFactory;

    /// The execution context type.
    type ExecutionCtx<'a>: Clone;

    /// Transaction type used by the executor.
    type Transaction;

    /// Receipt type produced by the executor.
    type Receipt;

    /// Reference to the EVM factory.
    fn evm_factory(&self) -> &Self::EvmFactory;

    /// Creates a block executor with the given EVM and execution context.
    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <Self::EvmFactory as GwynethEvmFactory>::Evm<DB, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> GwynethBlockExecutor<'a, <Self::EvmFactory as GwynethEvmFactory>::Evm<DB, I>>
    where
        DB: Database + ChainSwitchable + ParentLoadCheckpoints + 'a,
        I: Inspector<GwynethEvmContext<DB>> + 'a;
}

/// Simple factory for building Gwyneth block executors.
///
/// This factory stores a [`GwynethEvmFactory`] implementation and creates executors
/// using it.
#[derive(Debug, Clone)]
pub struct GwynethBlockExecutorFactory<F> {
    evm_factory: F,
}

impl<F> GwynethBlockExecutorFactory<F> {
    /// Create a new factory with the given EVM factory.
    pub const fn new(evm_factory: F) -> Self {
        Self { evm_factory }
    }

    /// Get a reference to the inner EVM factory.
    pub fn inner(&self) -> &F {
        &self.evm_factory
    }
}

impl<F: Default> Default for GwynethBlockExecutorFactory<F> {
    fn default() -> Self {
        Self::new(F::default())
    }
}

impl<F: GwynethEvmFactory + 'static> GwynethBlockExecutorFactoryTrait
    for GwynethBlockExecutorFactory<F>
{
    type EvmFactory = F;
    type ExecutionCtx<'a> = GwynethBlockExecutionCtx<'a>;
    type Transaction = (); // Placeholder - integrators should define this
    type Receipt = (); // Placeholder - integrators should define this

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: <Self::EvmFactory as GwynethEvmFactory>::Evm<DB, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> GwynethBlockExecutor<'a, <Self::EvmFactory as GwynethEvmFactory>::Evm<DB, I>>
    where
        DB: Database + ChainSwitchable + ParentLoadCheckpoints + 'a,
        I: Inspector<GwynethEvmContext<DB>> + 'a,
    {
        GwynethBlockExecutor::new(evm, ctx)
    }
}

/// Convenience methods for creating Gwyneth EVMs and executors.
impl<F: GwynethEvmFactory<Spec = SpecId>> GwynethBlockExecutorFactory<F> {
    /// Create an EVM without an inspector.
    pub fn create_evm<DB: Database + ChainSwitchable + ParentLoadCheckpoints>(
        &self,
        db: DB,
        env: EvmEnv<SpecId>,
    ) -> F::Evm<DB, NoOpInspector> {
        self.evm_factory.create_gwyneth_evm(db, env)
    }

    /// Create an EVM with an inspector.
    pub fn create_evm_with_inspector<
        DB: Database + ChainSwitchable + ParentLoadCheckpoints,
        I: Inspector<GwynethEvmContext<DB>>,
    >(
        &self,
        db: DB,
        env: EvmEnv<SpecId>,
        inspector: I,
    ) -> F::Evm<DB, I> {
        self.evm_factory.create_gwyneth_evm_with_inspector(db, env, inspector)
    }
}
