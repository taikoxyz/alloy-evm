#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(not(test))]
use gwyneth_types as _;

pub mod block;
pub mod factory;
pub mod halt_reason;
pub mod inspector;

pub use block::{
    GwynethBlockExecutionCtx, GwynethBlockExecutor, GwynethBlockExecutorFactory,
    GwynethBlockExecutorFactoryTrait,
};
pub use factory::{GwynethEvmFactory, GwynethEvmFactoryImpl};
pub use halt_reason::GwynethHaltReason;
pub use inspector::GwynethInspector;

use alloc::string::String;
use alloy_evm::{Evm, EvmEnv, MultiDatabase};
use alloy_primitives::Bytes;
use core::fmt::Debug;
use gwyneth_types::ChainState;
use gwyneth_detector::{DetectorConfig, GwynethDetector};
use gwyneth_engine::{
    GwynethCapabilities, GwynethContext, GwynethContextExt, GwynethHandler, GwynethHardFailure,
    GwynethPrecompileProvider, HardFailureInspector, L2OverlayDb, TrackingContextExt,
    TrackingJournal,
};
use revm::{
    context::{block::BlockEnv, cfg::CfgEnv, tx::TxEnv, Context},
    context_interface::{
        journaled_state::JournalTr,
        result::{EVMError, InvalidTransaction, ResultAndState},
        ContextSetters, ContextTr,
    },
    handler::{instructions::EthInstructions, EthFrame, Handler},
    inspector::{Inspector, InspectorHandler},
    interpreter::interpreter::EthInterpreter,
    primitives::{hardfork::SpecId, HashMap},
};

type InnerContext<DB> = GwynethContext<Context<BlockEnv, TxEnv, CfgEnv, DB, TrackingJournal<DB>>>;

type InnerEvm<DB, I> = revm::context::evm::Evm<
    InnerContext<DB>,
    GwynethInspector<I>,
    EthInstructions<EthInterpreter, InnerContext<DB>>,
    GwynethPrecompileProvider,
    EthFrame<EthInterpreter>,
>;

/// Gwyneth-flavoured EVM implementation that wraps the REVM context backed by the
/// Gwyneth tracking journal and precompile set.
#[allow(missing_debug_implementations)]
pub struct GwynethEvm<DB: MultiDatabase + gwyneth_types::ChainSwitchable, I> {
    inner: InnerEvm<DB, I>,
    blocks: HashMap<u64, BlockEnv>,
}

impl<DB: MultiDatabase + gwyneth_types::ChainSwitchable, I> GwynethEvm<DB, I>
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
        let chain_id = cfg_env.chain_id;
        let blocks = block_env;
        let block = blocks
            .get(&chain_id)
            .or_else(|| blocks.get(&0))
            .cloned()
            .unwrap_or_default();

        let ctx = Context::<BlockEnv, TxEnv, CfgEnv, DB, TrackingJournal<DB>>::new(db, cfg_env.spec)
            .with_cfg(cfg_env)
            .with_block(block);
        let gwyneth_ctx = GwynethContext::new(ctx, GwynethDetector::new(detector_config));
        let inspector = GwynethInspector::new(inspector, inspect);
        let inner = InnerEvm {
            ctx: gwyneth_ctx,
            inspector,
            instruction: EthInstructions::new_mainnet(),
            precompiles: GwynethPrecompileProvider::default(),
            frame_stack: Default::default(),
        };
        Self { inner, blocks }
    }

    fn ctx(&self) -> &InnerContext<DB> {
        &self.inner.ctx
    }

    fn ctx_mut(&mut self) -> &mut InnerContext<DB> {
        &mut self.inner.ctx
    }

    /// Get a reference to the Gwyneth journal.
    ///
    /// The journal tracks cross-chain calls, gas usage per chain,
    /// and other Gwyneth-specific execution data.
    pub fn gwyneth_journal(&self) -> &gwyneth_types::GwynethJournal {
        &self.inner.ctx.journal
    }

    /// Get a mutable reference to the Gwyneth journal.
    pub fn gwyneth_journal_mut(&mut self) -> &mut gwyneth_types::GwynethJournal {
        &mut self.inner.ctx.journal
    }

    /// Clone the current Gwyneth journal.
    ///
    /// This is useful for capturing the journal state after transaction execution.
    /// Note: This clones the journal without populating accounts_per_chain.
    /// Use `clone_gwyneth_journal_with_accounts` if you need per-chain account tracking.
    pub fn clone_gwyneth_journal(&self) -> gwyneth_types::GwynethJournal {
        self.inner.ctx.journal.clone()
    }

    /// Clone the Gwyneth journal and populate accounts_per_chain from the tracking journal.
    ///
    /// Phase 1 scaffolding does not yet populate per-chain account tracking; later slices
    /// will wire this through the gwyneth-owned tracking journal.
    pub fn clone_gwyneth_journal_with_accounts(&self) -> gwyneth_types::GwynethJournal {
        self.inner.ctx.journal.clone()
    }

    /// Drain callsite records captured by the always-on `JournalInspector`.
    ///
    /// This is a per-transaction buffer: `transact*` resets it before execution and callers
    /// should drain it after the transaction completes.
    pub fn take_callsite_records(&mut self) -> alloc::vec::Vec<gwyneth_types::oracle::CallsiteRecord> {
        self.inner.inspector.journal_mut().take_callsite_records()
    }

    /// Get a reference to the underlying database.
    pub fn db(&self) -> &DB {
        self.inner.ctx.base.journaled_state.db()
    }

    /// Get a mutable reference to the underlying database.
    pub fn db_mut(&mut self) -> &mut DB {
        self.inner.ctx.base.journaled_state.db_mut()
    }

    /// Get a reference to the tracking journal.
    ///
    /// The tracking journal captures per-chain state changes during cross-chain
    /// execution.
    pub fn tracking_journal(&self) -> &TrackingJournal<DB> {
        &self.inner.ctx.base.journaled_state
    }

    /// Get a mutable reference to the tracking journal.
    pub fn tracking_journal_mut(&mut self) -> &mut TrackingJournal<DB> {
        &mut self.inner.ctx.base.journaled_state
    }

    /// Override the parent (L1) chain id used for cross-chain classification.
    pub fn set_parent_chain_id(&mut self, parent_chain_id: Option<u64>) {
        self.inner.ctx.set_parent_chain_id(parent_chain_id);
    }

    /// Enable or disable xchain semantics.
    pub fn set_xchain_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_xchain_enabled(enabled);
    }

    /// Override capability toggles for chain switching and prewarming.
    pub fn set_capabilities(&mut self, capabilities: GwynethCapabilities) {
        self.inner.ctx.set_capabilities(capabilities);
    }

    /// Mark gwyneth config as present (used for tracking-only execution when xchain is disabled).
    pub fn set_gwyneth_configured(&mut self, configured: bool) {
        self.inner.ctx.set_gwyneth_configured(configured);
    }

    /// Mark extension oracle config as present (used for tracking-only execution when xchain is disabled).
    pub fn set_extension_oracle_configured(&mut self, configured: bool) {
        self.inner.ctx.set_extension_oracle_configured(configured);
    }

    /// Override allowed chain ids for XCALLOPTIONS routing.
    pub fn set_allowed_chain_ids(&mut self, allowed_chain_ids: alloc::vec::Vec<u64>) {
        self.inner.ctx.set_allowed_chain_ids(allowed_chain_ids);
    }

}

impl<DB, I> Evm for GwynethEvm<DB, I>
where
    DB: MultiDatabase
        + gwyneth_types::ChainSwitchable
        + gwyneth_types::ParentLoadCheckpoints
        + revm::Database,
    I: Inspector<InnerContext<DB>>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<<DB as revm::Database>::Error, InvalidTransaction>;
    type HaltReason = GwynethHaltReason;
    type Spec = SpecId;
    type Precompiles = GwynethPrecompileProvider;
    type Inspector = GwynethInspector<I>;

    fn blocks(&self) -> &revm::primitives::HashMap<u64, BlockEnv> {
        &self.blocks
    }

    fn chain_id(&self) -> u64 {
        self.ctx().base.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        mut tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        if tx.chain_id.is_none() {
            tx.chain_id = Some(self.chain_id());
        }
        let origin_chain_id = tx.chain_id.unwrap_or(self.chain_id());

        // Clear cross-transaction Gwyneth state and align the full execution context
        // (db/cfg/journal/local) to the transaction's origin chain before any inspector hooks run.
        self.inner.ctx.take_cross_chain_intent();
        self.inner.ctx.take_cross_chain_route();

        let mode_tracking_enabled = gwyneth_types::ExecutionMode::tracking_enabled(
            self.inner.ctx.is_xchain_enabled(),
            self.inner.ctx.parent_chain_id(),
            self.inner.ctx.gwyneth_configured(),
            self.inner.ctx.extension_oracle_configured(),
        );
        let is_direct = self.inner.ctx.parent_chain_id() == Some(origin_chain_id);
        let start_mode = gwyneth_types::ExecutionMode::from_context(
            origin_chain_id,
            self.inner.ctx.parent_chain_id(),
            is_direct,
            mode_tracking_enabled,
        );

        if self
            .inner
            .ctx
            .apply_chain_state(ChainState::new(origin_chain_id, origin_chain_id, start_mode))
            .is_err()
        {
            return Err(EVMError::Transaction(InvalidTransaction::InvalidChainId));
        }
        self.inner.frame_stack.clear();

        // Reset inspector state for the new transaction so hard-failure details can't leak.
        self.inner.inspector.reset_for_new_tx();

        self.inner.ctx.set_tx(tx);

        let mut handler = GwynethHandler::<_, Self::Error, EthFrame<EthInterpreter>>::new();

        // Mirror `InspectorHandler::inspect_run_without_catch_error`, but normalize hard failures
        // before post-execution output runs (the internal `FatalExternalError` would otherwise
        // panic when surfaced through `post_execution::output`).
        let init_and_floor_gas = handler.validate(&mut self.inner)?;
        let mut eip7702_refund = handler.pre_execution(&mut self.inner)? as i64;
        let mut frame_result = handler.inspect_execution(&mut self.inner, &init_and_floor_gas)?;

        let mut hard_failure: Option<GwynethHardFailure> = None;
        if let Some(details) = self.inner.inspector.take_hard_failure_details() {
            let gas_used = self.inner.ctx.tx().gas_limit;
            hard_failure = Some(GwynethHardFailure {
                chain_id: details.chain_id,
                opcode: details.opcode,
                reason: details.reason,
                gas_used,
                logs: alloc::vec::Vec::new(),
                output: Bytes::new(),
            });

            // Rewrite the internal `FatalExternalError` into a standard Halt.
            use revm::interpreter::InstructionResult;
            match &mut frame_result {
                revm::handler::FrameResult::Call(outcome) => {
                    outcome.result.result = InstructionResult::OutOfGas;
                    outcome.result.output = revm::primitives::Bytes::new();
                }
                revm::handler::FrameResult::Create(outcome) => {
                    outcome.result.result = InstructionResult::OutOfGas;
                    outcome.result.output = revm::primitives::Bytes::new();
                }
            }

            // Clear the forced context error so output can be produced normally.
            *self.inner.ctx.error() = Ok(());

            // Receipt contract: no refunds for hard failures.
            eip7702_refund = 0;

            // Per-chain attribution: full gas to the trigger chain, 0 elsewhere.
            let mut used = revm::primitives::HashMap::default();
            used.insert(details.chain_id, gas_used);
            self.inner
                .ctx
                .set_per_chain_gas(used, revm::primitives::HashMap::default());

            // Keep the journal's per-chain gas accounting consistent with the normalized surface.
            // Hard failures consume all gas and attribute it exclusively to the trigger chain.
            let journal = self.inner.ctx.gwyneth_journal_mut();
            journal.gas_used_per_chain.clear();
            journal.gas_used_per_chain.insert(details.chain_id, gas_used);
        }

        handler.post_execution(
            &mut self.inner,
            &mut frame_result,
            init_and_floor_gas,
            eip7702_refund,
        )?;

        let exec_result = handler.execution_result(&mut self.inner, frame_result)?;
        let mut exec_result = exec_result.map_haltreason(GwynethHaltReason::from);

        if let Some(hf) = hard_failure {
            exec_result = match exec_result {
                revm::context_interface::result::ExecutionResult::Halt { gas_used, .. } => {
                    revm::context_interface::result::ExecutionResult::Halt {
                        reason: GwynethHaltReason::GwynethHardFailure(hf),
                        gas_used,
                    }
                }
                other => other,
            };
        }

        let state = self.inner.journal_mut().finalize();
        Ok(ResultAndState::new(exec_result, state))
    }

    fn commit_state(&mut self, state: revm::state::EvmState)
    where
        Self::DB: revm::database_interface::DatabaseCommit,
    {
        let origin = self.inner.ctx.capture_chain_state();

        self.db_mut().commit(state);

        let mut pending = self
            .inner
            .ctx
            .tracking_journal_mut()
            .take_pending_commit_state();

        #[cfg(feature = "std")]
        if std::env::var_os("GWYNETH_DEBUG_MULTI_COMMIT").is_some() {
            let mut pending_chains: alloc::vec::Vec<u64> = pending.keys().copied().collect();
            pending_chains.sort_unstable();
            eprintln!(
                "[alloy-gwyneth-evm][commit_state] origin_chain={} pending_chains={:?}",
                origin.db_chain_id, pending_chains
            );
        }

        for (chain_id, changes) in pending.drain() {
            if changes.is_empty() {
                continue;
            }

            #[cfg(feature = "std")]
            if std::env::var_os("GWYNETH_DEBUG_MULTI_COMMIT").is_some() {
                eprintln!(
                    "[alloy-gwyneth-evm][commit_state] committing chain={} accounts={}",
                    chain_id,
                    changes.len()
                );
            }

            if self
                .inner
                .ctx
                .apply_chain_state(ChainState::new(chain_id, chain_id, origin.execution_mode))
                .is_ok()
            {
                self.db_mut().commit(changes);
            } else {
                debug_assert!(false, "commit_state: missing overlay for chain {}", chain_id);
            }
        }

        // Restore full chain state for subsequent calls.
        let _ = self.inner.ctx.apply_chain_state(origin);
    }

    fn transact_system_call(
        &mut self,
        caller: revm::primitives::Address,
        contract: revm::primitives::Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        use revm::handler::system_call::SystemCallTx;

        // Clear cross-transaction Gwyneth state and align the full execution context
        // (db/cfg/journal/local) to the system call target chain before any inspector hooks run.
        self.inner.ctx.take_cross_chain_intent();
        self.inner.ctx.take_cross_chain_route();

        let origin_chain_id = self.chain_id();
        let mode_tracking_enabled = gwyneth_types::ExecutionMode::tracking_enabled(
            self.inner.ctx.is_xchain_enabled(),
            self.inner.ctx.parent_chain_id(),
            self.inner.ctx.gwyneth_configured(),
            self.inner.ctx.extension_oracle_configured(),
        );
        let is_direct = self.inner.ctx.parent_chain_id() == Some(origin_chain_id);
        let start_mode = gwyneth_types::ExecutionMode::from_context(
            origin_chain_id,
            self.inner.ctx.parent_chain_id(),
            is_direct,
            mode_tracking_enabled,
        );

        let _ = self
            .inner
            .ctx
            .apply_chain_state(ChainState::new(origin_chain_id, origin_chain_id, start_mode));
        self.inner.frame_stack.clear();

        // Reset inspector state for the new system tx so hard-failure details can't leak.
        self.inner.inspector.reset_for_new_tx();

        self.inner
            .ctx
            .set_tx(TxEnv::new_system_tx_with_caller(caller, contract, data));

        let mut handler = GwynethHandler::<_, Self::Error, EthFrame<EthInterpreter>>::new();

        // Mirror `InspectorHandler::inspect_run_system_call`, but normalize hard failures before
        // output is computed.
        let init_and_floor_gas = revm::interpreter::InitialAndFloorGas::new(0, 0);
        let mut frame_result = handler.inspect_execution(&mut self.inner, &init_and_floor_gas)?;

        let mut hard_failure: Option<GwynethHardFailure> = None;
        if let Some(details) = self.inner.inspector.take_hard_failure_details() {
            let gas_used = self.inner.ctx.tx().gas_limit;
            hard_failure = Some(GwynethHardFailure {
                chain_id: details.chain_id,
                opcode: details.opcode,
                reason: details.reason,
                gas_used,
                logs: alloc::vec::Vec::new(),
                output: Bytes::new(),
            });

            use revm::interpreter::InstructionResult;
            match &mut frame_result {
                revm::handler::FrameResult::Call(outcome) => {
                    outcome.result.result = InstructionResult::OutOfGas;
                    outcome.result.output = revm::primitives::Bytes::new();
                }
                revm::handler::FrameResult::Create(outcome) => {
                    outcome.result.result = InstructionResult::OutOfGas;
                    outcome.result.output = revm::primitives::Bytes::new();
                }
            }

            *self.inner.ctx.error() = Ok(());

            let mut used = revm::primitives::HashMap::default();
            used.insert(details.chain_id, gas_used);
            self.inner
                .ctx
                .set_per_chain_gas(used, revm::primitives::HashMap::default());

            // Keep the journal's per-chain gas accounting consistent with the normalized surface.
            let journal = self.inner.ctx.gwyneth_journal_mut();
            journal.gas_used_per_chain.clear();
            journal.gas_used_per_chain.insert(details.chain_id, gas_used);
        }

        let exec_result = handler.execution_result(&mut self.inner, frame_result)?;
        let mut exec_result = exec_result.map_haltreason(GwynethHaltReason::from);

        if let Some(hf) = hard_failure {
            exec_result = match exec_result {
                revm::context_interface::result::ExecutionResult::Halt { gas_used, .. } => {
                    revm::context_interface::result::ExecutionResult::Halt {
                        reason: GwynethHaltReason::GwynethHardFailure(hf),
                        gas_used,
                    }
                }
                other => other,
            };
        }

        let state = self.inner.journal_mut().finalize();
        Ok(ResultAndState::new(exec_result, state))
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Self { inner, mut blocks } = self;
        let InnerEvm { ctx, .. } = inner;
        let GwynethContext { base, .. } = ctx;
        let Context { block, cfg, journaled_state, .. } = base;
        let db = journaled_state.into_db();
        blocks.insert(cfg.chain_id, block);
        (db, EvmEnv { block_env: blocks, cfg_env: cfg })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inner.inspector.set_user_enabled(enabled);
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
        let mut overlay = L2OverlayDb::new(gwyneth_types::L1_CHAIN_ID, l1_db);
        overlay.add_l2_overlay(chain_id, l2_db);
        let _ = overlay.switch_to_chain(chain_id);
        let mut blocks = revm::primitives::HashMap::default();
        blocks.insert(cfg_env.chain_id, block_env.clone());
        blocks.insert(0, block_env);
        Self::from_env(
            overlay,
            EvmEnv { block_env: blocks, cfg_env },
            inspector,
            DetectorConfig::default(),
            true,
        )
    }

    /// Switch the active overlay chain.
    pub fn switch_chain(&mut self, chain_id: u64) -> Result<(), String> {
        let mode = self.ctx().execution_mode();
        self.ctx_mut()
            .apply_chain_state(ChainState::new(chain_id, chain_id, mode))
            .map_err(|_| "Chain switch failed".to_string())
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
        let mut overlay = L2OverlayDb::new(gwyneth_types::L1_CHAIN_ID, l1_db);
        overlay.add_l2_overlay(chain_id, l2_db);
        let _ = overlay.switch_to_chain(chain_id);
        GwynethEvm::from_env(overlay, env, inspector, self.detector_config.clone(), true)
    }
}
