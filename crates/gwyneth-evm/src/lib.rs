#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(not(test))]
use gwyneth_types as _;

pub mod factory;
pub mod halt_reason;
pub mod inspector;

pub use factory::{GwynethEvmFactory, GwynethEvmFactoryImpl};
pub use halt_reason::GwynethHaltReason;
pub use inspector::GwynethInspector;

use alloc::string::String;
use alloy_evm::{Database, Evm, EvmEnv};
use alloy_primitives::Bytes;
use core::fmt::Debug;
use gwyneth_types::{ChainState, ExecutionSurface};
use gwyneth_detector::{DetectorConfig, GwynethDetector};
use gwyneth_engine::{
    GwynethCapabilities, GwynethContext, GwynethContextExt, GwynethHandler, GwynethHardFailure,
    GwynethPrecompileProvider, HardFailureInspector, L2OverlayDb, TrackingJournal,
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
    primitives::hardfork::SpecId,
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
pub struct GwynethRunner<DB: Database + gwyneth_types::ChainSwitchable, I> {
    inner: InnerEvm<DB, I>,
    surface: ExecutionSurface,
}

/// Convenience alias for the gwyneth execution runner.
///
/// This keeps existing call sites that refer to `GwynethEvm` compiling while the redesign
/// migrates the public surface toward the more explicit `GwynethRunner` naming.
pub type GwynethEvm<DB, I> = GwynethRunner<DB, I>;

impl<DB: Database + gwyneth_types::ChainSwitchable, I> GwynethRunner<DB, I>
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
    /// * `surface` - Execution surface kind (tx submission vs simulation-only)
    /// * `user_inspector_enabled` - Whether to enable the user inspector (gwyneth journal inspector remains active)
    pub fn from_env(
        db: DB,
        env: EvmEnv<SpecId>,
        inspector: I,
        detector_config: DetectorConfig,
        surface: ExecutionSurface,
        user_inspector_enabled: bool,
    ) -> Self {
        let EvmEnv { block_env, cfg_env } = env;
        let extension_oracle_address = detector_config.extension_oracle;

        let ctx = Context::<BlockEnv, TxEnv, CfgEnv, DB, TrackingJournal<DB>>::new(db, cfg_env.spec)
            .with_cfg(cfg_env)
            .with_block(block_env);
        let mut gwyneth_ctx = GwynethContext::new(ctx, GwynethDetector::new(detector_config));
        gwyneth_ctx.set_extension_oracle_address(extension_oracle_address);
        let inspector = GwynethInspector::new(inspector, user_inspector_enabled);
        let inner = InnerEvm {
            ctx: gwyneth_ctx,
            inspector,
            instruction: EthInstructions::new_mainnet(),
            precompiles: GwynethPrecompileProvider::default(),
            frame_stack: Default::default(),
        };
        Self { inner, surface }
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

    /// Override the treasury address used for basefee-burn forwarding (Phase 22.3).
    pub fn set_treasury_address(&mut self, treasury_address: Option<revm::primitives::Address>) {
        self.inner.ctx.set_treasury_address(treasury_address);
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

    /// Apply the standard gwyneth EVM configuration bundle for xchain-enforced execution
    /// (builder/stateless validation).
    pub fn configure_xchain_enforced(
        &mut self,
        parent_chain_id: Option<u64>,
        treasury_address: Option<revm::primitives::Address>,
        allowed_chain_ids: impl IntoIterator<Item = u64>,
    ) {
        self.configure_common(parent_chain_id, treasury_address, true, allowed_chain_ids);
    }

    /// Apply the standard gwyneth EVM configuration bundle for tracking-only execution with
    /// vanilla EVM semantics (`xchain_enabled=false`, no routing/interception).
    pub fn configure_tracking_only_vanilla(
        &mut self,
        parent_chain_id: Option<u64>,
        treasury_address: Option<revm::primitives::Address>,
        allowed_chain_ids: impl IntoIterator<Item = u64>,
    ) {
        self.configure_common(parent_chain_id, treasury_address, false, allowed_chain_ids);
    }

    fn configure_common(
        &mut self,
        parent_chain_id: Option<u64>,
        treasury_address: Option<revm::primitives::Address>,
        xchain_enabled: bool,
        allowed_chain_ids: impl IntoIterator<Item = u64>,
    ) {
        self.set_parent_chain_id(parent_chain_id);
        self.set_treasury_address(treasury_address);
        self.set_xchain_enabled(xchain_enabled);
        self.set_gwyneth_configured(true);
        self.set_extension_oracle_configured(true);

        let mut allowed_chain_ids: alloc::vec::Vec<u64> = allowed_chain_ids.into_iter().collect();
        allowed_chain_ids.sort_unstable();
        self.set_allowed_chain_ids(allowed_chain_ids);
    }

}

impl<DB, I> GwynethRunner<DB, I>
where
    DB: Database + gwyneth_types::ChainSwitchable + gwyneth_types::ParentLoadCheckpoints,
    I: Inspector<InnerContext<DB>>,
{
    fn reset_for_new_tx(
        &mut self,
        origin_chain_id: u64,
        strict_chain_id: bool,
    ) -> Result<(), EVMError<<DB as revm::Database>::Error, InvalidTransaction>> {
        // Clear cross-transaction Gwyneth state and align the full execution context
        // (db/cfg/journal/local) to the transaction's origin chain before any inspector hooks run.
        self.inner.ctx.clear_cross_chain_intents();
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

        // Reset per-tx tracking state so cross-chain diffs and forced-warm sets can't leak across
        // transactions.
        self.inner
            .journal_mut()
            .reset_for_new_tx(start_mode, origin_chain_id);

        let apply_result = self
            .inner
            .ctx
            .apply_chain_state(ChainState::new(origin_chain_id, origin_chain_id, start_mode));
        if strict_chain_id && apply_result.is_err() {
            return Err(EVMError::Transaction(InvalidTransaction::InvalidChainId));
        }

        self.inner.frame_stack.clear();

        // Reset inspector state for the new transaction so hard-failure details can't leak.
        self.inner.inspector.reset_for_new_tx();

        Ok(())
    }

    fn take_and_normalize_hard_failure(
        &mut self,
        frame_result: &mut revm::handler::FrameResult,
        gas_used: u64,
        eip7702_refund: Option<&mut i64>,
    ) -> Option<GwynethHardFailure> {
        let details = self.inner.inspector.take_hard_failure_details()?;
        let trigger_chain_id = details.chain_id;
        let hard_failure = GwynethHardFailure::from_details(details, gas_used);
        let normalized = gwyneth_types::normalize_superrevert(self.surface, gas_used);
        debug_assert_eq!(normalized.gas_used, gas_used, "SuperRevert normalization gas mismatch");

        // Rewrite the internal `FatalExternalError` into a standard Halt.
        use revm::interpreter::InstructionResult;
        match frame_result {
            revm::handler::FrameResult::Call(outcome) => {
                outcome.result.result = InstructionResult::OutOfGas;
                outcome.result.output = normalized.output.clone();
            }
            revm::handler::FrameResult::Create(outcome) => {
                outcome.result.result = InstructionResult::OutOfGas;
                outcome.result.output = normalized.output.clone();
            }
        }

        // Clear the forced context error so output can be produced normally.
        *self.inner.ctx.error() = Ok(());

        // Receipt contract: no refunds for hard failures.
        if let Some(refund) = eip7702_refund {
            *refund = normalized.refund;
        }

        // Per-chain attribution: full gas to the trigger chain, 0 elsewhere.
        let mut used = revm::primitives::HashMap::default();
        used.insert(trigger_chain_id, gas_used);
        self.inner
            .ctx
            .set_per_chain_gas(used, revm::primitives::HashMap::default());

        // Keep the journal's per-chain gas accounting consistent with the normalized surface.
        // Hard failures consume all gas and attribute it exclusively to the trigger chain.
        let journal = self.inner.ctx.gwyneth_journal_mut();
        journal.gas_used_per_chain.clear();
        journal.gas_used_per_chain.insert(trigger_chain_id, gas_used);

        Some(hard_failure)
    }

    fn attach_hard_failure_to_execution_result(
        exec_result: revm::context_interface::result::ExecutionResult<GwynethHaltReason>,
        hard_failure: Option<GwynethHardFailure>,
    ) -> revm::context_interface::result::ExecutionResult<GwynethHaltReason> {
        let Some(hf) = hard_failure else {
            return exec_result;
        };

        match exec_result {
            revm::context_interface::result::ExecutionResult::Halt { gas_used, .. } => {
                revm::context_interface::result::ExecutionResult::Halt {
                    reason: GwynethHaltReason::GwynethHardFailure(hf),
                    gas_used,
                }
            }
            other => other,
        }
    }
}

impl<DB, I> Evm for GwynethRunner<DB, I>
where
    DB: Database + gwyneth_types::ChainSwitchable + gwyneth_types::ParentLoadCheckpoints,
    I: Inspector<InnerContext<DB>>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<<DB as revm::Database>::Error, InvalidTransaction>;
    type HaltReason = GwynethHaltReason;
    type Spec = SpecId;
    type BlockEnv = BlockEnv;
    type Precompiles = GwynethPrecompileProvider;
    type Inspector = GwynethInspector<I>;

    fn block(&self) -> &Self::BlockEnv {
        &self.ctx().base.block
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

        self.reset_for_new_tx(origin_chain_id, true)?;

        self.inner.ctx.set_tx(tx);

        let mut handler = GwynethHandler::<_, Self::Error, EthFrame<EthInterpreter>>::new();

        // Mirror `InspectorHandler::inspect_run_without_catch_error`, but normalize hard failures
        // before post-execution output runs (the internal `FatalExternalError` would otherwise
        // panic when surfaced through `post_execution::output`).
        let init_and_floor_gas = handler.validate(&mut self.inner)?;
        let mut eip7702_refund = handler.pre_execution(&mut self.inner)? as i64;
        let mut frame_result = handler.inspect_execution(&mut self.inner, &init_and_floor_gas)?;

        let hard_failure = self.take_and_normalize_hard_failure(
            &mut frame_result,
            self.inner.ctx.tx().gas_limit,
            Some(&mut eip7702_refund),
        );

        handler.post_execution(
            &mut self.inner,
            &mut frame_result,
            init_and_floor_gas,
            eip7702_refund,
        )?;

        let exec_result = handler.execution_result(&mut self.inner, frame_result)?;
        let mut exec_result = exec_result.map_haltreason(GwynethHaltReason::from);

        exec_result = Self::attach_hard_failure_to_execution_result(exec_result, hard_failure);

        let state = self.inner.journal_mut().finalize();
        Ok(ResultAndState::new(exec_result, state))
    }

    fn transact_system_call(
        &mut self,
        caller: revm::primitives::Address,
        contract: revm::primitives::Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        use revm::handler::system_call::SystemCallTx;

        let origin_chain_id = self.chain_id();
        self.reset_for_new_tx(origin_chain_id, false)?;

        self.inner
            .ctx
            .set_tx(TxEnv::new_system_tx_with_caller(caller, contract, data));

        let mut handler = GwynethHandler::<_, Self::Error, EthFrame<EthInterpreter>>::new();

        // Mirror `InspectorHandler::inspect_run_system_call`, but normalize hard failures before
        // output is computed.
        let init_and_floor_gas = revm::interpreter::InitialAndFloorGas::new(0, 0);
        let mut frame_result = handler.inspect_execution(&mut self.inner, &init_and_floor_gas)?;

        let hard_failure =
            self.take_and_normalize_hard_failure(&mut frame_result, self.inner.ctx.tx().gas_limit, None);

        let exec_result = handler.execution_result(&mut self.inner, frame_result)?;
        let mut exec_result = exec_result.map_haltreason(GwynethHaltReason::from);

        exec_result = Self::attach_hard_failure_to_execution_result(exec_result, hard_failure);

        let state = self.inner.journal_mut().finalize();
        Ok(ResultAndState::new(exec_result, state))
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Self { inner, .. } = self;
        let InnerEvm { ctx, .. } = inner;
        let GwynethContext { base, .. } = ctx;
        let Context { block, cfg, journaled_state, .. } = base;
        let db = journaled_state.into_db();
        (db, EvmEnv { block_env: block, cfg_env: cfg })
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

impl<L1DB, L2DB, I> GwynethRunner<L2OverlayDb<L1DB, L2DB>, I>
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
        Self::from_env(
            overlay,
            EvmEnv { block_env, cfg_env },
            inspector,
            DetectorConfig::default(),
            ExecutionSurface::TxSubmission,
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
    ) -> GwynethRunner<L2OverlayDb<L1DB, L2DB>, I>
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
    ) -> GwynethRunner<L2OverlayDb<L1DB, L2DB>, I>
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
        GwynethRunner::from_env(
            overlay,
            env,
            inspector,
            self.detector_config.clone(),
            ExecutionSurface::TxSubmission,
            true,
        )
    }
}
