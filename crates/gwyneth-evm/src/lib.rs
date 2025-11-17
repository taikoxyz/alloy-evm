#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(not(test))]
use gwyneth_types as _;

pub mod block;
pub mod factory;

pub use block::{
    GwynethBlockExecutionCtx, GwynethBlockExecutor, GwynethBlockExecutorFactory,
    GwynethBlockExecutorFactoryTrait,
};
pub use factory::{GwynethEvmFactory, GwynethEvmFactoryImpl};

use alloc::string::String;
use alloy_evm::{Database, Evm, EvmEnv};
use alloy_primitives::{Address, Bytes};
use core::fmt::Debug;
use gwyneth_detector::{DetectorConfig, GwynethDetector};
use gwyneth_engine::{
    GwynethContext, GwynethHandler, GwynethPrecompileProvider, L2OverlayDb, TrackingJournal,
};
use revm::{
    context::{block::BlockEnv, cfg::CfgEnv, tx::TxEnv, Context},
    context_interface::{
        journaled_state::JournalTr,
        result::{EVMError, HaltReason, InvalidTransaction, ResultAndState},
        ContextSetters, ContextTr,
    },
    handler::{instructions::EthInstructions, EthFrame, Handler},
    inspector::{Inspector, InspectorHandler},
    interpreter::interpreter::EthInterpreter,
    primitives::hardfork::SpecId,
    SystemCallEvm,
};

type InnerContext<DB> = GwynethContext<Context<BlockEnv, TxEnv, CfgEnv, DB, TrackingJournal<DB>>>;

type InnerEvm<DB, I> = revm::context::evm::Evm<
    InnerContext<DB>,
    I,
    EthInstructions<EthInterpreter, InnerContext<DB>>,
    GwynethPrecompileProvider,
    EthFrame<EthInterpreter>,
>;

/// Gwyneth-flavoured EVM implementation that wraps the REVM context backed by the
/// Gwyneth tracking journal and precompile set.
#[allow(missing_debug_implementations)]
pub struct GwynethEvm<DB: Database, I> {
    inner: InnerEvm<DB, I>,
    inspect: bool,
}

impl<DB: Database + gwyneth_types::ChainSwitchable, I> GwynethEvm<DB, I>
where
    I: Inspector<InnerContext<DB>>,
{
    /// Create a new Gwyneth EVM from environment configuration.
    ///
    /// # Arguments
    ///
    /// * `db` - The database (must implement `ChainSwitchable` for multi-chain support)
    /// * `env` - The EVM environment (block and config)
    /// * `inspector` - The inspector for tracing/debugging
    /// * `detector_config` - Configuration for cross-chain call detection
    /// * `inspect` - Whether to enable the inspector during transaction execution
    pub fn from_env(
        db: DB,
        env: EvmEnv<SpecId>,
        inspector: I,
        detector_config: DetectorConfig,
        inspect: bool,
    ) -> Self {
        let EvmEnv { block_env, cfg_env } = env;
        let ctx =
            Context::<BlockEnv, TxEnv, CfgEnv, DB, TrackingJournal<DB>>::new(db, cfg_env.spec);
        let ctx = ctx.with_block(block_env).with_cfg(cfg_env);
        let gwyneth_ctx = GwynethContext::new(ctx, GwynethDetector::new(detector_config));
        let inner = InnerEvm {
            ctx: gwyneth_ctx,
            inspector,
            instruction: EthInstructions::new_mainnet(),
            precompiles: GwynethPrecompileProvider::default(),
            frame_stack: Default::default(),
        };
        Self { inner, inspect }
    }

    fn ctx(&self) -> &InnerContext<DB> {
        &self.inner.ctx
    }

    fn ctx_mut(&mut self) -> &mut InnerContext<DB> {
        &mut self.inner.ctx
    }
}

impl<DB, I> Evm for GwynethEvm<DB, I>
where
    DB: Database + gwyneth_types::ChainSwitchable,
    I: Inspector<InnerContext<DB>>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error, InvalidTransaction>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type Precompiles = GwynethPrecompileProvider;
    type Inspector = I;

    fn block(&self) -> &BlockEnv {
        &self.ctx().base.block
    }

    fn chain_id(&self) -> u64 {
        self.ctx().base.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.inner.ctx.set_tx(tx);
        let mut handler = GwynethHandler::<_, Self::Error, EthFrame<EthInterpreter>>::new();
        let exec_result = if self.inspect {
            handler.inspect_run(&mut self.inner)?
        } else {
            handler.run(&mut self.inner)?
        };
        let state = self.inner.journal_mut().finalize();
        Ok(ResultAndState::new(exec_result, state))
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        self.inner.system_call_with_caller(caller, contract, data)
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let InnerEvm { ctx, .. } = self.inner;
        let GwynethContext { base, .. } = ctx;
        let Context { block, cfg, journaled_state, .. } = base;
        let db = journaled_state.into_database();
        (db, EvmEnv { block_env: block, cfg_env: cfg })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        let InnerEvm { ctx, inspector, precompiles, .. } = &self.inner;
        (ctx.base.journaled_state.db(), inspector, precompiles)
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        let InnerEvm { ctx, inspector, precompiles, .. } = &mut self.inner;
        (ctx.base.journaled_state.db_mut(), inspector, precompiles)
    }
}

impl<L1DB, L2DB, I> GwynethEvm<L2OverlayDb<L1DB, L2DB>, I>
where
    L1DB: revm::Database + Debug,
    L2DB: revm::Database + Debug,
    L1DB::Error: Debug + Send + Sync + 'static,
    L2DB::Error: Debug + Send + Sync + 'static,
    I: Inspector<InnerContext<L2OverlayDb<L1DB, L2DB>>>,
{
    /// Construct a Gwyneth EVM that already has an L2 overlay configured.
    #[allow(dead_code)]
    pub fn new_with_l2_overlay(
        l1_db: L1DB,
        l2_db: L2DB,
        chain_id: u64,
        inspector: I,
        block_env: BlockEnv,
        cfg_env: CfgEnv,
    ) -> Self {
        let mut overlay = L2OverlayDb::new(l1_db);
        overlay.add_l2_overlay(chain_id, l2_db);
        let _ = overlay.switch_to_chain(chain_id);
        Self::from_env(
            overlay,
            EvmEnv { block_env, cfg_env },
            inspector,
            DetectorConfig::default(),
            true,
        )
    }

    /// Switch the active overlay chain.
    pub fn switch_chain(&mut self, chain_id: u64) -> Result<(), String> {
        self.ctx_mut().base.journaled_state.db_mut().switch_to_chain(chain_id)
    }

    /// Return the current active chain id.
    pub fn current_chain_id(&self) -> u64 {
        self.ctx().base.journaled_state.db().current_chain_id()
    }
}

// Note: The `GwynethEvmFactory` trait is now defined in the `factory` module.
// Use `GwynethEvmFactoryImpl` for the concrete factory implementation.

/// Convenience alias for the Gwyneth context backing the inspector.
pub type GwynethEvmContext<DB> = InnerContext<DB>;

/// Extension trait that makes building overlay-enabled EVMs ergonomic.
pub trait GwynethEvmExt {
    /// Create a Gwyneth EVM that has both L1 and L2 databases attached.
    fn create_l2_evm<L1DB, L2DB, I>(
        &self,
        l1_db: L1DB,
        l2_db: L2DB,
        chain_id: u64,
        env: EvmEnv<SpecId>,
        inspector: I,
    ) -> GwynethEvm<L2OverlayDb<L1DB, L2DB>, I>
    where
        L1DB: revm::Database + Debug,
        L2DB: revm::Database + Debug,
        L1DB::Error: Debug + Send + Sync + 'static,
        L2DB::Error: Debug + Send + Sync + 'static,
        I: Inspector<InnerContext<L2OverlayDb<L1DB, L2DB>>>;
}

impl GwynethEvmExt for GwynethEvmFactoryImpl {
    fn create_l2_evm<L1DB, L2DB, I>(
        &self,
        l1_db: L1DB,
        l2_db: L2DB,
        chain_id: u64,
        env: EvmEnv<SpecId>,
        inspector: I,
    ) -> GwynethEvm<L2OverlayDb<L1DB, L2DB>, I>
    where
        L1DB: revm::Database + Debug,
        L2DB: revm::Database + Debug,
        L1DB::Error: Debug + Send + Sync + 'static,
        L2DB::Error: Debug + Send + Sync + 'static,
        I: Inspector<InnerContext<L2OverlayDb<L1DB, L2DB>>>,
    {
        let mut overlay = L2OverlayDb::new(l1_db);
        overlay.add_l2_overlay(chain_id, l2_db);
        let _ = overlay.switch_to_chain(chain_id);
        GwynethEvm::from_env(overlay, env, inspector, self.detector_config.clone(), true)
    }
}
