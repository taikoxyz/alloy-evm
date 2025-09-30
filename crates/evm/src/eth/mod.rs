//! Ethereum EVM implementation.

use crate::{env::EvmEnv, evm::EvmFactory, precompiles::PrecompilesMap, Evm, MultiDatabase};
use alloy_primitives::Bytes;
use core::{
    fmt::Debug,
    ops::{Deref, DerefMut},
};
use revm::{
    context::{BlockEnv, CfgEnv, ContextSetters, Evm as RevmEvm, TxEnv},
    context_interface::result::{EVMError, HaltReason, ResultAndState},
    handler::{instructions::EthInstructions, EthFrame, EthPrecompiles, PrecompileProvider},
    inspector::{inspectors::GwynethCompositeInspector, NoOpInspector},
    interpreter::{interpreter::EthInterpreter, InterpreterResult},
    precompile::{PrecompileSpecId, Precompiles},
    primitives::{hardfork::SpecId, ChainAddress, HashMap, MultiChainTxKind as TxKind},
    AutoSetupBuilder, Context, ExecuteEvm, InspectEvm, InspectSystemCallEvm, Inspector,
    SystemCallEvm,
};

mod block;
pub use block::*;

pub mod dao_fork;
pub mod eip6110;
pub mod receipt_builder;
pub mod spec;

/// The Ethereum EVM context type.
pub type EthEvmContext<DB> = Context<BlockEnv, TxEnv, CfgEnv, DB>;

/// Helper builder to construct `EthEvm` instances in a unified way.
#[expect(missing_debug_implementations)]
#[derive(Clone)]
pub struct EthEvmBuilder<DB: MultiDatabase, I = NoOpInspector> {
    db: DB,
    env: EvmEnv,
    inspector: I,
    inspect: bool,
    precompiles: Option<PrecompilesMap>,
}

impl<DB: MultiDatabase> EthEvmBuilder<DB, NoOpInspector> {
    /// Creates a builder from the provided `EvmEnv` and database.
    pub fn new(db: DB, env: EvmEnv) -> Self {
        Self { db, env, inspector: NoOpInspector {}, inspect: false, precompiles: None }
    }
}

impl<DB: MultiDatabase, I> EthEvmBuilder<DB, I> {
    /// Sets a custom inspector
    pub fn inspector<J>(self, inspector: J) -> EthEvmBuilder<DB, J> {
        EthEvmBuilder {
            db: self.db,
            env: self.env,
            inspector,
            inspect: self.inspect,
            precompiles: self.precompiles,
        }
    }

    /// Sets a custom inspector and enables invoking it during transaction execution.
    pub fn activate_inspector<J>(self, inspector: J) -> EthEvmBuilder<DB, J> {
        self.inspector(inspector).inspect()
    }

    /// Sets whether to invoke the inspector during transaction execution.
    pub fn set_inspect(mut self, inspect: bool) -> Self {
        self.inspect = inspect;
        self
    }

    /// Enables invoking the inspector during transaction execution.
    pub fn inspect(self) -> Self {
        self.set_inspect(true)
    }

    /// Overrides the precompiles map. If not provided, it will be derived from the `SpecId` in
    /// `CfgEnv`.
    pub fn precompiles(mut self, precompiles: PrecompilesMap) -> Self {
        self.precompiles = Some(precompiles);
        self
    }

    /// Builds the `EthEvm` instance.
    pub fn build(self) -> EthEvm<DB, I, PrecompilesMap>
    where
        I: Inspector<EthEvmContext<DB>>,
    {
        let EthEvmBuilder { db, env, inspector, inspect, precompiles } = self;
        let EvmEnv { cfg_env, block_env } = env;

        let precompiles = match precompiles {
            Some(p) => p,
            None => PrecompilesMap::from_static(Precompiles::new(
                PrecompileSpecId::from_spec_id(cfg_env.spec),
                cfg_env.xchain,
            )),
        };

        let gwyneth_inspector = GwynethCompositeInspector::wrap(inspector);
        let inner = Context::mainnet()
            .with_blocks(block_env)
            .with_cfg(cfg_env)
            .with_db(db)
            .build_gwyneth_with_inspector(gwyneth_inspector)
            .with_precompiles(precompiles);

        EthEvm { inner, inspect }
    }
}

/// Applies multi-chain configuration overrides from the given `EvmEnv` to the provided
/// `EthEvmContext`.
pub fn apply_multichain_overrides<DB: MultiDatabase>(ctx: &mut EthEvmContext<DB>, env: &EvmEnv) {
    ctx.set_blocks(env.block_env.clone());
    ctx.modify_cfg(|cfg| {
        *cfg = env.cfg_env.clone();
    });
}

/// Ethereum EVM implementation.
///
/// This is a wrapper type around the `revm` ethereum evm with optional [`Inspector`] (tracing)
/// support. [`Inspector`] support is configurable at runtime because it's part of the underlying
/// [`RevmEvm`] type.
#[expect(missing_debug_implementations)]
pub struct EthEvm<DB: MultiDatabase, I, PRECOMPILE = EthPrecompiles> {
    inner: RevmEvm<
        EthEvmContext<DB>,
        GwynethCompositeInspector<I>,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame,
    >,
    inspect: bool,
}

impl<DB: MultiDatabase, I, PRECOMPILE> EthEvm<DB, I, PRECOMPILE> {
    /// Creates a new Ethereum EVM instance.
    ///
    /// The `inspect` argument determines whether the configured [`Inspector`] of the given
    /// [`RevmEvm`] should be invoked on [`Evm::transact`].
    pub const fn new(
        evm: RevmEvm<
            EthEvmContext<DB>,
            GwynethCompositeInspector<I>,
            EthInstructions<EthInterpreter, EthEvmContext<DB>>,
            PRECOMPILE,
            EthFrame,
        >,
        inspect: bool,
    ) -> Self {
        Self { inner: evm, inspect }
    }

    /// Consumes self and return the inner EVM instance.
    pub fn into_inner(
        self,
    ) -> RevmEvm<
        EthEvmContext<DB>,
        GwynethCompositeInspector<I>,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame,
    > {
        self.inner
    }

    /// Provides a reference to the EVM context.
    pub const fn ctx(&self) -> &EthEvmContext<DB> {
        &self.inner.ctx
    }

    /// Provides a mutable reference to the EVM context.
    pub fn ctx_mut(&mut self) -> &mut EthEvmContext<DB> {
        &mut self.inner.ctx
    }
}

impl<DB: MultiDatabase, I, PRECOMPILE> Deref for EthEvm<DB, I, PRECOMPILE> {
    type Target = EthEvmContext<DB>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB: MultiDatabase, I, PRECOMPILE> DerefMut for EthEvm<DB, I, PRECOMPILE> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

impl<DB, I, PRECOMPILE> Evm for EthEvm<DB, I, PRECOMPILE>
where
    DB: MultiDatabase,
    I: Inspector<EthEvmContext<DB>>,
    PRECOMPILE: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    type DB = DB;
    type Tx = TxEnv;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type Precompiles = PRECOMPILE;
    type Inspector = I;

    fn blocks(&self) -> &HashMap<u64, BlockEnv> {
        &self.inner.ctx.block
    }

    fn chain_id(&self) -> u64 {
        self.inner.ctx.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        mut tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        // For legacy transactions without a chain_id, use the default from config
        if tx.chain_id.is_none() {
            let default_chain_id = if let Some(parent_chain_id) = self.cfg.parent_chain_id {
                parent_chain_id
            } else {
                self.cfg.chain_id
            };
            tx.chain_id = Some(default_chain_id);

            // Also update the caller and call addresses
            tx.caller = ChainAddress::new(default_chain_id, tx.caller.1);
            if let TxKind::Call(ref mut addr) = tx.kind {
                *addr = ChainAddress::new(default_chain_id, addr.1);
            }
        }

        // Set chain_ids from available blocks
        tx.chain_ids = Some(self.blocks().keys().cloned().collect());

        if self.inspect {
            self.inner.inspect_tx(tx)
        } else {
            self.inner.transact(tx)
        }
    }

    fn transact_system_call(
        &mut self,
        caller: ChainAddress,
        contract: ChainAddress,
        data: Bytes,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        // When inspection is enabled, use the inspect path so multi-chain gas tracking is populated.
        if self.inspect {
            self.inner.inspect_system_call_with_caller(caller, contract, data)
        } else {
            self.inner.system_call_with_caller(caller, contract, data)
        }
    }

    fn db_mut(&mut self) -> &mut Self::DB {
        &mut self.inner.ctx.journaled_state.database
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context { block: block_env, cfg: cfg_env, journaled_state, .. } = self.inner.ctx;

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (
            &self.inner.ctx.journaled_state.database,
            &self.inner.inspector.custom_inspector,
            &self.inner.precompiles,
        )
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.ctx.journaled_state.database,
            &mut self.inner.inspector.custom_inspector,
            &mut self.inner.precompiles,
        )
    }
}

/// Factory producing [`EthEvm`].
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct EthEvmFactory;

impl EvmFactory for EthEvmFactory {
    type Evm<DB: MultiDatabase, I: Inspector<EthEvmContext<DB>>> = EthEvm<DB, I, Self::Precompiles>;
    type Context<DB: MultiDatabase> = Context<BlockEnv, TxEnv, CfgEnv, DB>;
    type Tx = TxEnv;
    type Error<DBError: core::error::Error + Send + Sync + 'static> = EVMError<DBError>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type Precompiles = PrecompilesMap;

    fn create_evm<DB: MultiDatabase>(&self, db: DB, input: EvmEnv) -> Self::Evm<DB, NoOpInspector> {
        let env_clone = input.clone();
        let mut evm = EthEvmBuilder::new(db, env_clone).build();
        apply_multichain_overrides(evm.ctx_mut(), &input);
        evm
    }

    fn create_evm_with_inspector<DB: MultiDatabase, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let env_clone = input.clone();
        let mut evm = EthEvmBuilder::new(db, env_clone).activate_inspector(inspector).build();
        apply_multichain_overrides(evm.ctx_mut(), &input);
        evm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use revm::{
        database::{EmptyDB, MultiEmptyDB},
        inspector::NoOpInspector,
        primitives::{hardfork::SpecId, HashMap},
    };

    #[test]
    fn test_precompiles_with_correct_spec() {
        // create tests where precompile should be available for later specs but not earlier ones
        let specs_to_test = [
            // MODEXP (0x05) was added in Byzantium, should not exist in Frontier
            (
                address!("0x0000000000000000000000000000000000000005"),
                SpecId::FRONTIER,  // Early spec - should NOT have this precompile
                SpecId::BYZANTIUM, // Later spec - should have this precompile
                "MODEXP",
            ),
            // BLAKE2F (0x09) was added in Istanbul, should not exist in Byzantium
            (
                address!("0x0000000000000000000000000000000000000009"),
                SpecId::BYZANTIUM, // Early spec - should NOT have this precompile
                SpecId::ISTANBUL,  // Later spec - should have this precompile
                "BLAKE2F",
            ),
        ];

        for (precompile_addr, early_spec, later_spec, name) in specs_to_test {
            let mut early_cfg_env = CfgEnv::default();
            early_cfg_env.spec = early_spec;
            early_cfg_env.chain_id = 1;

            let mut block_map = HashMap::default();
            block_map.insert(1, BlockEnv::default());
            let early_env = EvmEnv { block_env: block_map, cfg_env: early_cfg_env };
            let factory = EthEvmFactory;
            let mut multi_db = MultiEmptyDB::new();
            multi_db.add_chain(1, EmptyDB::default());
            let mut early_evm = factory.create_evm(multi_db, early_env);

            // precompile should NOT be available in early spec
            assert!(
                early_evm.precompiles_mut().get(&precompile_addr).is_none(),
                "{name} precompile at {precompile_addr:?} should NOT be available for early spec {early_spec:?}"
            );

            let mut later_cfg_env = CfgEnv::default();
            later_cfg_env.spec = later_spec;
            later_cfg_env.chain_id = 1;

            let mut block_map = HashMap::default();
            block_map.insert(1, BlockEnv::default());
            let later_env = EvmEnv { block_env: block_map, cfg_env: later_cfg_env };
            let mut multi_db = MultiEmptyDB::new();
            multi_db.add_chain(1, EmptyDB::default());
            let mut later_evm = factory.create_evm(multi_db, later_env);

            // precompile should be available in later spec
            assert!(
                later_evm.precompiles_mut().get(&precompile_addr).is_some(),
                "{name} precompile at {precompile_addr:?} should be available for later spec {later_spec:?}"
            );
        }
    }

    #[test]
    fn factory_applies_multichain_overrides() {
        let mut cfg_env = CfgEnv::default();
        cfg_env.chain_id = 100;
        cfg_env.xchain = true;
        cfg_env.parent_chain_id = Some(1);
        cfg_env.extension_oracle = Some(address!("0x0000000000000000000000000000000000000100"));
        cfg_env.gwyneth = Some(address!("0x0000000000000000000000000000000000000200"));

        let mut block_map = HashMap::default();
        block_map.insert(cfg_env.chain_id, BlockEnv::default());
        block_map.insert(1, BlockEnv::default());
        let expected_blocks = block_map.clone();

        let env = EvmEnv { block_env: block_map, cfg_env: cfg_env.clone() };

        let factory = EthEvmFactory;
        let mut multi_db = MultiEmptyDB::new();
        multi_db.add_chain(1, EmptyDB::default());
        multi_db.add_chain(cfg_env.chain_id, EmptyDB::default());

        let evm = factory.create_evm(multi_db, env.clone());
        let ctx = evm.ctx();

        let cfg = &ctx.cfg;
        assert!(cfg.xchain, "xchain flag should propagate to context");
        assert_eq!(cfg.parent_chain_id, env.cfg_env.parent_chain_id);
        assert_eq!(cfg.extension_oracle, env.cfg_env.extension_oracle);
        assert_eq!(cfg.gwyneth, env.cfg_env.gwyneth);
        assert_eq!(cfg.chain_id, env.cfg_env.chain_id);

        for chain_id in expected_blocks.keys() {
            assert!(ctx.block.contains_key(chain_id), "missing block env for chain {}", chain_id);
        }
    }

    #[test]
    fn inspector_factory_keeps_overrides() {
        let mut cfg_env = CfgEnv::default();
        cfg_env.chain_id = 200;
        cfg_env.xchain = true;
        cfg_env.parent_chain_id = Some(1);
        cfg_env.extension_oracle = Some(address!("0x0000000000000000000000000000000000000300"));

        let mut block_map = HashMap::default();
        block_map.insert(cfg_env.chain_id, BlockEnv::default());
        block_map.insert(1, BlockEnv::default());

        let env = EvmEnv { block_env: block_map, cfg_env: cfg_env.clone() };

        let factory = EthEvmFactory;
        let mut multi_db = MultiEmptyDB::new();
        multi_db.add_chain(1, EmptyDB::default());
        multi_db.add_chain(cfg_env.chain_id, EmptyDB::default());

        let inspector = NoOpInspector {};
        let evm = factory.create_evm_with_inspector(multi_db, env.clone(), inspector);
        let ctx = evm.ctx();

        assert!(ctx.cfg.xchain, "xchain flag should persist when using inspector path");
        assert_eq!(ctx.cfg.parent_chain_id, env.cfg_env.parent_chain_id);
        assert!(ctx.block.contains_key(&cfg_env.chain_id));
        assert!(ctx.block.contains_key(&1));
    }
}
