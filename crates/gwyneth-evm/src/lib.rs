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
    GwynethContext, GwynethContextExt, GwynethHandler, GwynethPrecompileProvider, L2OverlayDb,
    TrackingContextExt, TrackingJournal,
};
use gwyneth_types::{ChainState, ExecutionMode, L1_CHAIN_ID};
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
pub struct GwynethEvm<DB: Database + gwyneth_types::ChainSwitchable, I> {
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

    const fn ctx(&self) -> &InnerContext<DB> {
        &self.inner.ctx
    }

    const fn ctx_mut(&mut self) -> &mut InnerContext<DB> {
        &mut self.inner.ctx
    }

    /// Get a reference to the Gwyneth journal.
    ///
    /// The journal tracks cross-chain calls, gas usage per chain,
    /// and other Gwyneth-specific execution data.
    pub const fn gwyneth_journal(&self) -> &gwyneth_types::GwynethJournal {
        &self.inner.ctx.journal
    }

    /// Get a mutable reference to the Gwyneth journal.
    pub const fn gwyneth_journal_mut(&mut self) -> &mut gwyneth_types::GwynethJournal {
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

    /// Clone the Gwyneth journal and include any accounts_per_chain updates.
    ///
    /// Note: accounts_per_chain is populated during `commit_multi_chain`, so ensure
    /// you commit before cloning if you need per-chain account tracking.
    pub fn clone_gwyneth_journal_with_accounts(&self) -> gwyneth_types::GwynethJournal {
        self.inner.ctx.journal.clone()
    }

    /// Update the configured parent (L1) chain id.
    pub fn set_parent_chain_id(&mut self, parent_chain_id: Option<u64>) {
        self.inner.ctx.set_parent_chain_id(parent_chain_id);
    }

    /// Update the allowed chain ids for XCALLOPTIONS routing.
    pub fn set_allowed_chain_ids(&mut self, allowed_chain_ids: Vec<u64>) {
        self.inner.ctx.set_allowed_chain_ids(allowed_chain_ids);
    }

    /// Enable or disable cross-chain semantics.
    pub fn set_xchain_enabled(&mut self, enabled: bool) {
        self.inner.ctx.set_xchain_enabled(enabled);
    }

    /// Update whether the gwyneth config is present for tracking-only execution.
    pub fn set_gwyneth_configured(&mut self, configured: bool) {
        self.inner.ctx.set_gwyneth_configured(configured);
    }

    /// Update whether the extension oracle is configured for tracking-only execution.
    pub fn set_extension_oracle_configured(&mut self, configured: bool) {
        self.inner.ctx.set_extension_oracle_configured(configured);
    }

    /// Update the treasury address for basefee-burn forwarding.
    pub fn set_treasury_address(&mut self, treasury_address: Option<Address>) {
        self.inner.ctx.set_treasury_address(treasury_address);
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
    pub const fn tracking_journal(&self) -> &TrackingJournal<DB> {
        &self.inner.ctx.base.journaled_state
    }

    /// Get a mutable reference to the tracking journal.
    pub const fn tracking_journal_mut(&mut self) -> &mut TrackingJournal<DB> {
        &mut self.inner.ctx.base.journaled_state
    }

    /// Commit state changes with multi-chain support.
    ///
    /// This method properly handles cross-chain state changes by:
    /// 1. Getting per-chain state from the tracking journal
    /// 2. Switching to each chain and committing its changes
    /// 3. Restoring the original chain
    ///
    /// This is the correct way to commit state after executing transactions
    /// that may have cross-chain effects.
    ///
    /// # Arguments
    ///
    /// * `state` - The state to commit (used as fallback if no per-chain tracking)
    pub fn commit_multi_chain(&mut self, state: revm::state::EvmState)
    where
        DB: revm::database_interface::DatabaseCommit,
    {
        // Drain staged per-chain state captured by finalize.
        let pending = self.tracking_journal_mut().take_pending_commit_state();
        let original_chain_id = self.db().current_chain_id();
        {
            let journal = self.gwyneth_journal_mut();
            for address in state.keys() {
                journal.record_account_modified(original_chain_id, *address);
            }
            for (chain_id, changes) in pending.iter() {
                for address in changes.keys() {
                    journal.record_account_modified(*chain_id, *address);
                }
            }
        }

        // Commit active-chain changes.
        self.db_mut().commit(state);

        if !pending.is_empty() {
            for (chain_id, changes) in pending {
                if self.db_mut().switch_to_chain(chain_id).is_ok() {
                    self.db_mut().commit(changes);
                }
            }
            let _ = self.db_mut().switch_to_chain(original_chain_id);
        }
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
    type BlockEnv = BlockEnv;
    type Precompiles = GwynethPrecompileProvider;
    type Inspector = I;

    fn block(&self) -> &Self::BlockEnv {
        &self.ctx().base.block
    }

    fn chain_id(&self) -> u64 {
        self.ctx().base.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        let origin_chain_id =
            tx.chain_id.ok_or_else(|| EVMError::Transaction(InvalidTransaction::MissingChainId))?;
        let start_mode = match self.inner.ctx.execution_mode() {
            ExecutionMode::L1Simulated => ExecutionMode::L1Simulated,
            _ => {
                if self.inner.ctx.parent_chain_id().is_some_and(|parent| parent == origin_chain_id)
                {
                    ExecutionMode::L1Direct
                } else {
                    ExecutionMode::L2
                }
            }
        };

        // Clear per-tx gwyneth state and align the context to the transaction origin chain.
        self.inner.ctx.clear_cross_chain_intents();
        let _ = self.inner.ctx.take_cross_chain_route();
        self.inner.ctx.clear_post_frame_actions();
        self.inner.ctx.tracking_journal_mut().reset_for_new_tx(start_mode, origin_chain_id);
        if self
            .inner
            .ctx
            .apply_chain_state(ChainState::new(origin_chain_id, origin_chain_id, start_mode))
            .is_err()
        {
            return Err(EVMError::Transaction(InvalidTransaction::InvalidChainId));
        }
        self.inner.frame_stack.clear();

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

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec, Self::BlockEnv>) {
        let InnerEvm { ctx, .. } = self.inner;
        let GwynethContext { base, .. } = ctx;
        let Context { block, cfg, journaled_state, .. } = base;
        let db = journaled_state.into_db();
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
        let mut overlay = L2OverlayDb::new(L1_CHAIN_ID, l1_db);
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
        let mut overlay = L2OverlayDb::new(L1_CHAIN_ID, l1_db);
        overlay.add_l2_overlay(chain_id, l2_db);
        let _ = overlay.switch_to_chain(chain_id);
        GwynethEvm::from_env(overlay, env, inspector, self.detector_config.clone(), true)
    }
}
