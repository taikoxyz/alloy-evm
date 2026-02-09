#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(not(test))]
use gwyneth_types as _;

pub mod factory;
pub mod fees;
pub mod halt_reason;
pub mod inspector;

pub use factory::GwynethEvmFactoryImpl;
pub use fees::{compute_multichain_fees, tx_fee_fields_from_tx, FeeError, MultichainFees, TxFeeFields};
pub use halt_reason::GwynethHaltReason;
pub use inspector::GwynethInspector;

use alloc::string::String;
use alloy_evm::{Database, Evm, EvmEnv};
use alloy_primitives::{map::HashMap, Bytes};
use core::fmt::Debug;
use gwyneth_types::{
    normalize_superrevert, ChainState, ExecutionSurface, TreasuryForwarding, TreasuryForwardingMode,
};
use gwyneth_detector::DetectorConfig;
use gwyneth_engine::{
    build_l2_overlay_db_adapter, GwynethChain, GwynethContext, GwynethHardFailure, GwynethLocal,
    GwynethPrecompileProvider, HardFailureInspector, L2OverlayDb, TrackingJournal,
};
use revm::{
    context::{block::BlockEnv, cfg::CfgEnv, tx::TxEnv, Context},
    context_interface::{
        block::Block as _,
        journaled_state::JournalTr,
        result::{EVMError, InvalidTransaction, ResultAndState},
        ContextTr, LocalContextTr as _,
    },
    handler::{instructions::EthInstructions, EthFrame},
    InspectEvm, InspectSystemCallEvm, Inspector,
    interpreter::interpreter::EthInterpreter,
    primitives::hardfork::SpecId,
};

type InnerContext<DB> = GwynethContext<DB, TrackingJournal<DB>>;

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
    per_chain_basefee: HashMap<u64, u64>,
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

        let base =
            Context::<BlockEnv, TxEnv, CfgEnv, DB, TrackingJournal<DB>>::new(db, cfg_env.spec)
                .with_cfg(cfg_env)
                .with_block(block_env);
        let mut gwyneth_ctx = base
            .with_chain(GwynethChain::default())
            .with_local(GwynethLocal::default());
        gwyneth_ctx
            .chain_mut()
            .set_extension_oracle_address(extension_oracle_address);
        let inspector = GwynethInspector::new(inspector, user_inspector_enabled);
        let inner = InnerEvm {
            ctx: gwyneth_ctx,
            inspector,
            instruction: EthInstructions::new_mainnet(),
            precompiles: GwynethPrecompileProvider::default(),
            frame_stack: Default::default(),
        };
        Self { inner, surface, per_chain_basefee: HashMap::default() }
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
        self.inner.ctx.chain().gwyneth_journal()
    }

    /// Get a mutable reference to the Gwyneth journal.
    pub fn gwyneth_journal_mut(&mut self) -> &mut gwyneth_types::GwynethJournal {
        self.inner.ctx.chain_mut().gwyneth_journal_mut()
    }

    /// Clone the current Gwyneth journal.
    ///
    /// This is useful for capturing the journal state after transaction execution.
    /// Note: This clones the journal without populating accounts_per_chain.
    /// Use `clone_gwyneth_journal_with_accounts` if you need per-chain account tracking.
    pub fn clone_gwyneth_journal(&self) -> gwyneth_types::GwynethJournal {
        self.gwyneth_journal().clone()
    }

    /// Clone the Gwyneth journal and populate accounts_per_chain from the tracking journal.
    ///
    /// Phase 1 scaffolding does not yet populate per-chain account tracking; later slices
    /// will wire this through the gwyneth-owned tracking journal.
    pub fn clone_gwyneth_journal_with_accounts(&self) -> gwyneth_types::GwynethJournal {
        self.gwyneth_journal().clone()
    }

    /// Get a reference to the underlying database.
    pub fn db(&self) -> &DB {
        self.inner.ctx.db()
    }

    /// Get a mutable reference to the underlying database.
    pub fn db_mut(&mut self) -> &mut DB {
        self.inner.ctx.db_mut()
    }

    /// Get a reference to the tracking journal.
    ///
    /// The tracking journal captures per-chain state changes during cross-chain
    /// execution.
    pub fn tracking_journal(&self) -> &TrackingJournal<DB> {
        &self.inner.ctx.journaled_state
    }

    /// Get a mutable reference to the tracking journal.
    pub fn tracking_journal_mut(&mut self) -> &mut TrackingJournal<DB> {
        &mut self.inner.ctx.journaled_state
    }

    /// Override the parent (L1) chain id used for cross-chain classification.
    pub fn set_parent_chain_id(&mut self, parent_chain_id: Option<u64>) {
        self.inner.ctx.chain_mut().set_parent_chain_id(parent_chain_id);
    }

    /// Override the treasury address used for basefee-burn forwarding (Phase 22.3).
    pub fn set_treasury_address(&mut self, treasury_address: Option<revm::primitives::Address>) {
        self.inner.ctx.chain_mut().set_treasury_address(treasury_address);
    }

    /// Override allowed chain ids for XCALLOPTIONS routing.
    pub fn set_allowed_chain_ids(&mut self, allowed_chain_ids: alloc::vec::Vec<u64>) {
        self.inner.ctx.chain_mut().set_allowed_chain_ids(allowed_chain_ids);
    }

    /// Set the per-chain basefee source used by fee attribution.
    pub fn set_per_chain_basefees(
        &mut self,
        per_chain_basefee: impl IntoIterator<Item = (u64, u64)>,
    ) {
        self.per_chain_basefee.clear();
        for (chain_id, basefee) in per_chain_basefee {
            self.per_chain_basefee.insert(chain_id, basefee);
        }
    }

    /// Apply the standard gwyneth EVM configuration bundle (builder/stateless validation).
    pub fn configure_xchain_enforced(
        &mut self,
        parent_chain_id: Option<u64>,
        treasury_address: Option<revm::primitives::Address>,
        allowed_chain_ids: impl IntoIterator<Item = u64>,
    ) {
        self.configure_common(parent_chain_id, treasury_address, allowed_chain_ids);
    }

    fn configure_common(
        &mut self,
        parent_chain_id: Option<u64>,
        treasury_address: Option<revm::primitives::Address>,
        allowed_chain_ids: impl IntoIterator<Item = u64>,
    ) {
        self.set_parent_chain_id(parent_chain_id);
        self.set_treasury_address(treasury_address);

        let mut allowed_chain_ids: alloc::vec::Vec<u64> = allowed_chain_ids.into_iter().collect();
        allowed_chain_ids.sort_unstable();
        self.set_allowed_chain_ids(allowed_chain_ids);

        // Default source-of-truth for single-chain execution paths. Multi-chain builder/stateless
        // paths must overwrite this map with per-chain values before execution.
        self.set_per_chain_basefees([(
            self.inner.ctx.cfg.chain_id,
            self.inner.ctx.block.basefee,
        )]);
    }

}

impl<DB, I> GwynethRunner<DB, I>
where
    DB: Database + gwyneth_types::ChainSwitchable + gwyneth_types::ParentLoadCheckpoints,
    I: Inspector<InnerContext<DB>>,
{
    fn ensure_origin_chain_alignment(
        &mut self,
        origin_chain_id: u64,
        context: &'static str,
    ) -> Result<(), EVMError<<DB as revm::Database>::Error, InvalidTransaction>> {
        if self.db().current_chain_id() == origin_chain_id {
            return Ok(());
        }

        let is_direct = self.inner.ctx.chain().parent_chain_id() == Some(origin_chain_id);
        let start_mode = gwyneth_types::ExecutionMode::from_context(
            origin_chain_id,
            self.inner.ctx.chain().parent_chain_id(),
            is_direct,
        );

        gwyneth_engine::apply_chain_state(
            &mut self.inner.ctx,
            ChainState::new(origin_chain_id, origin_chain_id, start_mode),
        )
        .map_err(|_| {
                EVMError::Custom(alloc::format!(
                    "{context}: failed to restore origin chain (wanted={origin_chain_id}, got={})",
                    self.db().current_chain_id()
                ))
            })?;

        if self.db().current_chain_id() != origin_chain_id {
            return Err(EVMError::Custom(alloc::format!(
                "{context}: origin chain misaligned after apply_chain_state (wanted={origin_chain_id}, got={})",
                self.db().current_chain_id()
            )));
        }

        Ok(())
    }

    fn reset_for_new_tx(
        &mut self,
        origin_chain_id: u64,
        strict_chain_id: bool,
    ) -> Result<(), EVMError<<DB as revm::Database>::Error, InvalidTransaction>> {
        // Clear cross-transaction Gwyneth state and align the full execution context
        // (db/cfg/journal/local) to the transaction's origin chain before any inspector hooks run.
        self.inner.ctx.local.clear();

        let is_direct = self.inner.ctx.chain().parent_chain_id() == Some(origin_chain_id);
        let start_mode = gwyneth_types::ExecutionMode::from_context(
            origin_chain_id,
            self.inner.ctx.chain().parent_chain_id(),
            is_direct,
        );

        // Reset per-tx tracking state so cross-chain diffs and forced-warm sets can't leak across
        // transactions.
        self.inner
            .journal_mut()
            .reset_for_new_tx(start_mode, origin_chain_id);

        let apply_result = gwyneth_engine::apply_chain_state(
            &mut self.inner.ctx,
            ChainState::new(origin_chain_id, origin_chain_id, start_mode),
        );
        if strict_chain_id && apply_result.is_err() {
            return Err(EVMError::Transaction(InvalidTransaction::InvalidChainId));
        }

        self.inner.frame_stack.clear();

        // Reset inspector state for the new transaction so hard-failure details can't leak.
        self.inner.inspector.reset_for_new_tx();

        Ok(())
    }

    fn apply_superrevert_fee_surface_to_balance_deltas(
        &mut self,
        origin_chain_id: u64,
        expected_gas_used: u64,
        actual_gas_used: u64,
    ) -> Result<(), EVMError<<DB as revm::Database>::Error, InvalidTransaction>> {
        if expected_gas_used <= actual_gas_used {
            return Ok(());
        }

        // Hard-failure contract: full gas must be charged even if pre-execution refunds (e.g.
        // EIP-7702) were applied by the handler.
        let missing_gas = expected_gas_used - actual_gas_used;

        self.ensure_origin_chain_alignment(
            origin_chain_id,
            "apply_superrevert_fee_surface_to_balance_deltas",
        )?;

        use revm::context_interface::{Block as _, Cfg as _, Transaction as _};
        let basefee = self.inner.ctx.block().basefee() as u128;
        let effective_gas_price = self.inner.ctx.tx().effective_gas_price(basefee);

        let spec_id: SpecId = self.inner.ctx.cfg().spec().into();
        let coinbase_gas_price = if spec_id.is_enabled_in(SpecId::LONDON) {
            effective_gas_price.saturating_sub(basefee)
        } else {
            effective_gas_price
        };

        let missing_fee =
            revm::primitives::U256::from(effective_gas_price.saturating_mul(missing_gas as u128));
        let missing_beneficiary_fee =
            revm::primitives::U256::from(coinbase_gas_price.saturating_mul(missing_gas as u128));

        let caller = self.inner.ctx.tx().caller();
        let beneficiary = self.inner.ctx.block().beneficiary();

        {
            let mut caller_account = self.inner.ctx.journal_mut().load_account_mut(caller)?.data;
            if !caller_account.decr_balance(missing_fee) {
                return Err(EVMError::Custom(alloc::format!(
                    "superrevert fee normalization underflow while charging caller: missing_fee={missing_fee}"
                )));
            }
        }

        if !missing_beneficiary_fee.is_zero() {
            let mut beneficiary_account = self
                .inner
                .ctx
                .journal_mut()
                .load_account_mut(beneficiary)?
                .data;
            if !beneficiary_account.incr_balance(missing_beneficiary_fee) {
                return Err(EVMError::Custom(alloc::format!(
                    "superrevert fee normalization overflow while crediting beneficiary: missing_beneficiary_fee={missing_beneficiary_fee}"
                )));
            }
        }

        Ok(())
    }

    fn apply_multichain_fee_surface_to_balance_deltas(
        &mut self,
        origin_chain_id: u64,
        gas_used: u64,
        desired_total_fee: revm::primitives::U256,
        desired_total_tip: revm::primitives::U256,
    ) -> Result<(), EVMError<<DB as revm::Database>::Error, InvalidTransaction>> {
        use revm::context_interface::{Block as _, Cfg as _, Transaction as _};

        self.ensure_origin_chain_alignment(
            origin_chain_id,
            "apply_multichain_fee_surface_to_balance_deltas",
        )?;

        let basefee = self.inner.ctx.block().basefee() as u128;
        let effective_gas_price = self.inner.ctx.tx().effective_gas_price(basefee);
        let spec_id: SpecId = self.inner.ctx.cfg().spec().into();
        let actual_tip_per_gas = if spec_id.is_enabled_in(SpecId::LONDON) {
            effective_gas_price.checked_sub(basefee).ok_or_else(|| {
                EVMError::Custom(alloc::format!(
                    "multichain fee normalization invalid gas price surface: effective_gas_price={effective_gas_price} basefee={basefee}"
                ))
            })?
        } else {
            effective_gas_price
        };

        let gas_used_u256 = revm::primitives::U256::from(gas_used);
        let actual_total_fee = revm::primitives::U256::from(effective_gas_price)
            .checked_mul(gas_used_u256)
            .ok_or_else(|| {
                EVMError::Custom(alloc::format!(
                    "multichain fee normalization overflow: actual_total_fee (effective_gas_price={effective_gas_price}, gas_used={gas_used})"
                ))
            })?;
        let actual_total_tip = revm::primitives::U256::from(actual_tip_per_gas)
            .checked_mul(gas_used_u256)
            .ok_or_else(|| {
                EVMError::Custom(alloc::format!(
                    "multichain fee normalization overflow: actual_total_tip (actual_tip_per_gas={actual_tip_per_gas}, gas_used={gas_used})"
                ))
            })?;

        let caller = self.inner.ctx.tx().caller();
        let beneficiary = self.inner.ctx.block().beneficiary();

        if desired_total_fee > actual_total_fee {
            let delta = desired_total_fee - actual_total_fee;
            let mut caller_account = self.inner.ctx.journal_mut().load_account_mut(caller)?.data;
            if !caller_account.decr_balance(delta) {
                return Err(EVMError::Custom(alloc::format!(
                    "multichain fee normalization underflow while charging caller: delta={delta}"
                )));
            }
        } else if actual_total_fee > desired_total_fee {
            let delta = actual_total_fee - desired_total_fee;
            let mut caller_account = self.inner.ctx.journal_mut().load_account_mut(caller)?.data;
            if !caller_account.incr_balance(delta) {
                return Err(EVMError::Custom(alloc::format!(
                    "multichain fee normalization overflow while refunding caller: delta={delta}"
                )));
            }
        }

        if desired_total_tip > actual_total_tip {
            let delta = desired_total_tip - actual_total_tip;
            let mut beneficiary_account = self
                .inner
                .ctx
                .journal_mut()
                .load_account_mut(beneficiary)?
                .data;
            if !beneficiary_account.incr_balance(delta) {
                return Err(EVMError::Custom(alloc::format!(
                    "multichain fee normalization overflow while crediting beneficiary: delta={delta}"
                )));
            }
        } else if actual_total_tip > desired_total_tip {
            let delta = actual_total_tip - desired_total_tip;
            let mut beneficiary_account = self
                .inner
                .ctx
                .journal_mut()
                .load_account_mut(beneficiary)?
                .data;
            if !beneficiary_account.decr_balance(delta) {
                return Err(EVMError::Custom(alloc::format!(
                    "multichain fee normalization underflow while debiting beneficiary: delta={delta}"
                )));
            }
        }

        Ok(())
    }

    fn take_hard_failure(&mut self, gas_used: u64) -> Option<(u64, GwynethHardFailure)> {
        let details = self.inner.inspector.take_hard_failure_details()?;
        let trigger_chain_id = details.chain_id;
        let hard_failure = GwynethHardFailure::from_details(details, gas_used);

        // Keep the journal's per-chain gas accounting consistent with the normalized surface.
        // Hard failures consume all gas and attribute it exclusively to the trigger chain.
        let journal = self.inner.ctx.chain_mut().gwyneth_journal_mut();
        journal.gas_used_per_chain.clear();
        journal.gas_used_per_chain.insert(trigger_chain_id, gas_used);

        Some((trigger_chain_id, hard_failure))
    }

    fn apply_treasury_forwarding_post_run(
        &mut self,
        origin_chain_id: u64,
        gas_used: u64,
    ) -> Result<(), EVMError<<DB as revm::Database>::Error, InvalidTransaction>> {
        // Phase 60.8: fee attribution and treasury forwarding use per-chain charged gas/basefee
        // surfaces and fail closed on invalid/missing fee inputs.
        //
        // Must not run on simulation-only surfaces and must not run for system calls (explicitly fee-free).
        if self.surface != ExecutionSurface::TxSubmission {
            return Ok(());
        }

        let tx = self.inner.ctx.tx();
        if tx.caller == revm::handler::system_call::SYSTEM_ADDRESS {
            return Ok(());
        }

        let Some(treasury_address) = self.inner.ctx.chain().treasury_address() else {
            return Ok(());
        };

        let mut gas_used_per_chain =
            self.inner.ctx.chain().gwyneth_journal().gas_used_per_chain.clone();
        let tracked_total = gas_used_per_chain.values().try_fold(0u64, |acc, &value| {
            acc.checked_add(value).ok_or_else(|| {
                EVMError::Custom(alloc::format!(
                    "multichain fee attribution overflow while summing per-chain gas: acc={acc} value={value}"
                ))
            })
        })?;
        if tracked_total < gas_used {
            let remainder = gas_used - tracked_total;
            let origin_prev = gas_used_per_chain.get(&origin_chain_id).copied().unwrap_or(0);
            let origin_next = origin_prev.checked_add(remainder).ok_or_else(|| {
                EVMError::Custom(alloc::format!(
                    "multichain fee attribution overflow while assigning origin-chain gas remainder: origin_prev={origin_prev} remainder={remainder}"
                ))
            })?;
            gas_used_per_chain.insert(origin_chain_id, origin_next);
        } else if tracked_total > gas_used {
            // The execution result gas surface is post-refund while internal per-chain tracking
            // can still carry a higher metered total. Normalize the refund delta on the origin
            // chain, which is the single settlement surface for fee accounting.
            let refund_delta = tracked_total - gas_used;
            let origin_prev = gas_used_per_chain.get(&origin_chain_id).copied().unwrap_or(0);
            let origin_next = origin_prev.checked_sub(refund_delta).ok_or_else(|| {
                EVMError::Custom(alloc::format!(
                    "multichain fee attribution cannot apply refund delta to origin chain: origin_prev={origin_prev} refund_delta={refund_delta} tracked_total={tracked_total} execution_result_gas={gas_used}"
                ))
            })?;
            gas_used_per_chain.insert(origin_chain_id, origin_next);
        }

        let tx_fee_fields = tx_fee_fields_from_tx(tx).map_err(|err| {
            EVMError::Custom(alloc::format!(
                "multichain fee attribution rejected tx fee fields (tx_type={}): {err:?}",
                tx.tx_type
            ))
        })?;
        let fees = compute_multichain_fees(&gas_used_per_chain, &self.per_chain_basefee, tx_fee_fields)
            .map_err(|err| EVMError::Custom(alloc::format!("multichain fee attribution failed: {err:?}")))?;

        if fees.gas_used_total != gas_used {
            return Err(EVMError::Custom(alloc::format!(
                "multichain fee attribution gas mismatch: per_chain_total={} execution_result_gas={gas_used}",
                fees.gas_used_total
            )));
        }

        self.apply_multichain_fee_surface_to_balance_deltas(
            origin_chain_id,
            gas_used,
            fees.total_fee,
            fees.total_tip,
        )?;

        let origin_basefee = self.inner.ctx.block().basefee();
        let basefee_burn = fees.total_basefee_burn;

        let mode = match self.inner.ctx.chain().parent_chain_id() {
            Some(parent) if parent == origin_chain_id => TreasuryForwardingMode::ExposeToHost,
            _ => TreasuryForwardingMode::CreditedToTreasury,
        };

        self.inner.ctx.chain_mut().gwyneth_journal_mut().treasury_forwarding = Some(TreasuryForwarding {
            origin_chain_id,
            treasury_address,
            origin_basefee,
            gas_used: fees.gas_used_total,
            basefee_burn,
            mode,
        });

        if mode == TreasuryForwardingMode::CreditedToTreasury && !basefee_burn.is_zero() {
            self.ensure_origin_chain_alignment(origin_chain_id, "apply_treasury_forwarding_post_run")?;
            self.inner
                .journal_mut()
                .balance_incr(treasury_address, basefee_burn)?;
        }

        Ok(())
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
        &self.ctx().block
    }

    fn chain_id(&self) -> u64 {
        self.ctx().cfg.chain_id
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

        let mut exec_result = self.inner.inspect_one_tx(tx)?;

        // Normalize gwyneth hard failures after core has produced a stable terminal halt, but
        // before we finalize state (so per-chain attribution and fee corrections are captured).
        let expected_gas_used = self.inner.ctx.tx().gas_limit;
        let hard_failure = match self.take_hard_failure(expected_gas_used) {
            Some((_trigger_chain_id, hard_failure)) => {
                let normalized = normalize_superrevert(self.surface, expected_gas_used);
                let actual_gas_used = exec_result.gas_used();
                self.apply_superrevert_fee_surface_to_balance_deltas(
                    origin_chain_id,
                    normalized.gas_used,
                    actual_gas_used,
                )?;

                if normalized.gas_used != actual_gas_used {
                    if let revm::context_interface::result::ExecutionResult::Halt { gas_used, .. } =
                        &mut exec_result
                    {
                        *gas_used = normalized.gas_used;
                    }
                }

                Some(hard_failure)
            }
            None => None,
        };

        let mut exec_result = exec_result.map_haltreason(GwynethHaltReason::from);
        exec_result = Self::attach_hard_failure_to_execution_result(exec_result, hard_failure);

        self.apply_treasury_forwarding_post_run(origin_chain_id, exec_result.gas_used())?;

        let state = self.inner.journal_mut().finalize();
        Ok(ResultAndState::new(exec_result, state))
    }

    fn transact_system_call(
        &mut self,
        caller: revm::primitives::Address,
        contract: revm::primitives::Address,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        let origin_chain_id = self.chain_id();
        self.reset_for_new_tx(origin_chain_id, false)?;

        let mut exec_result =
            self.inner
                .inspect_one_system_call_with_caller(caller, contract, data)?;

        let expected_gas_used = self.inner.ctx.tx().gas_limit;
        let hard_failure = match self.take_hard_failure(expected_gas_used) {
            Some((_trigger_chain_id, hard_failure)) => {
                let normalized = normalize_superrevert(self.surface, expected_gas_used);
                let actual_gas_used = exec_result.gas_used();
                if normalized.gas_used != actual_gas_used {
                    if let revm::context_interface::result::ExecutionResult::Halt { gas_used, .. } =
                        &mut exec_result
                    {
                        *gas_used = normalized.gas_used;
                    }
                }
                Some(hard_failure)
            }
            None => None,
        };

        let exec_result = exec_result.map_haltreason(GwynethHaltReason::from);
        let exec_result = Self::attach_hard_failure_to_execution_result(exec_result, hard_failure);

        let state = self.inner.journal_mut().finalize();
        Ok(ResultAndState::new(exec_result, state))
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Self { inner, .. } = self;
        let InnerEvm { ctx, .. } = inner;
        let Context { block, cfg, journaled_state, .. } = ctx;
        let db = journaled_state.into_db();
        (db, EvmEnv { block_env: block, cfg_env: cfg })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inner.inspector.set_user_enabled(enabled);
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        let InnerEvm { ctx, inspector, precompiles, .. } = &self.inner;
        (ctx.db(), inspector, precompiles)
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        let InnerEvm { ctx, inspector, precompiles, .. } = &mut self.inner;
        (ctx.db_mut(), inspector, precompiles)
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
        let overlay = build_l2_overlay_db_adapter(
            gwyneth_types::L1_CHAIN_ID,
            l1_db,
            [(chain_id, l2_db)],
            chain_id,
        )
        .expect("create overlay db adapter");
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
        let mode = self.ctx().local().execution_mode();
        gwyneth_engine::apply_chain_state(
            self.ctx_mut(),
            ChainState::new(chain_id, chain_id, mode),
        )
            .map_err(|_| "Chain switch failed".to_string())
    }

    /// Return the current active chain id.
    pub fn current_chain_id(&self) -> u64 {
        self.db().current_chain_id()
    }
}

// Note: Use `GwynethEvmFactoryImpl::for_surface(..)` + `alloy_evm::evm::BoundedEvmFactory<DB>`
// to express gwyneth-only DB bounds without mirroring the canonical `EvmFactory` trait.

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
        let overlay = build_l2_overlay_db_adapter(
            gwyneth_types::L1_CHAIN_ID,
            l1_db,
            [(chain_id, l2_db)],
            chain_id,
        )
        .expect("create overlay db adapter");
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
