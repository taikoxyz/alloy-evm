//! Ethereum EVM implementation.

use crate::{env::EvmEnv, evm::EvmFactory, precompiles::PrecompilesMap, Evm, MultiDatabase};
use alloy_primitives::Bytes;
use core::{
    fmt::Debug,
    ops::{Deref, DerefMut},
};
use revm::{
    context::{BlockEnv, CfgEnv, Evm as RevmEvm, TxEnv},
    context_interface::result::{EVMError, HaltReason, ResultAndState},
    handler::{instructions::EthInstructions, EthFrame, PrecompileProvider},
    inspector::NoOpInspector,
    interpreter::{interpreter::EthInterpreter, InterpreterResult},
    precompile::{PrecompileSpecId, Precompiles},
    primitives::{hardfork::SpecId, Address, HashMap},
    Context, ExecuteEvm, InspectEvm, InspectSystemCallEvm, Inspector, SystemCallEvm,
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
            None => PrecompilesMap::from_static(Precompiles::new(PrecompileSpecId::from_spec_id(
                cfg_env.spec,
            ))),
        };

        let chain_id = cfg_env.chain_id;
        let blocks = block_env;
        let block = blocks
            .get(&chain_id)
            .or_else(|| blocks.get(&0))
            .cloned()
            .unwrap_or_default();

        let ctx =
            Context::<BlockEnv, TxEnv, CfgEnv, DB>::new(db, cfg_env.spec).with_cfg(cfg_env).with_block(block);
        let inner = RevmEvm {
            ctx,
            inspector,
            instruction: EthInstructions::new_mainnet(),
            precompiles,
            frame_stack: Default::default(),
        };

        EthEvm { inner, blocks, inspect }
    }
}

/// Applies multi-chain configuration overrides from the given `EvmEnv`.
pub fn apply_multichain_overrides<DB, I, PRECOMPILE>(
    evm: &mut EthEvm<DB, I, PRECOMPILE>,
    env: &EvmEnv,
) where
    DB: MultiDatabase,
    I: Inspector<EthEvmContext<DB>>,
    PRECOMPILE: PrecompileProvider<EthEvmContext<DB>, Output = InterpreterResult>,
{
    evm.blocks = env.block_env.clone();

    let chain_id = env.cfg_env.chain_id;
    let maybe_block = evm
        .blocks
        .get(&chain_id)
        .or_else(|| evm.blocks.get(&0))
        .cloned();
    if let Some(block) = maybe_block {
        evm.inner.ctx.modify_block(|b| *b = block);
    }

    evm.inner.ctx.modify_cfg(|cfg| *cfg = env.cfg_env.clone());
    let _ = evm.inner.precompiles.set_spec(env.cfg_env.spec);
}

/// Ethereum EVM implementation.
///
/// This is a wrapper type around the `revm` ethereum evm with optional [`Inspector`] (tracing)
/// support. [`Inspector`] support is configurable at runtime because it's part of the underlying
/// [`RevmEvm`] type.
#[expect(missing_debug_implementations)]
pub struct EthEvm<DB: MultiDatabase, I, PRECOMPILE = PrecompilesMap> {
    inner: RevmEvm<
        EthEvmContext<DB>,
        I,
        EthInstructions<EthInterpreter, EthEvmContext<DB>>,
        PRECOMPILE,
        EthFrame,
    >,
    blocks: HashMap<u64, BlockEnv>,
    inspect: bool,
}

impl<DB: MultiDatabase, I, PRECOMPILE> EthEvm<DB, I, PRECOMPILE> {
    /// Creates a new Ethereum EVM instance.
    ///
    /// The `inspect` argument determines whether the configured [`Inspector`] of the given
    /// [`RevmEvm`] should be invoked on [`Evm::transact`].
    pub fn new(
        evm: RevmEvm<
            EthEvmContext<DB>,
            I,
            EthInstructions<EthInterpreter, EthEvmContext<DB>>,
            PRECOMPILE,
            EthFrame,
        >,
        blocks: HashMap<u64, BlockEnv>,
        inspect: bool,
    ) -> Self {
        Self { inner: evm, blocks, inspect }
    }

    /// Consumes self and return the inner EVM instance.
    pub fn into_inner(
        self,
    ) -> RevmEvm<
        EthEvmContext<DB>,
        I,
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
        &self.blocks
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
            tx.chain_id = Some(self.chain_id());
        }

        if self.inspect {
            self.inner.inspect_tx(tx)
        } else {
            self.inner.transact(tx)
        }
    }

    fn transact_system_call(
        &mut self,
        caller: Address,
        contract: Address,
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

        let mut blocks = self.blocks;
        blocks.insert(cfg_env.chain_id, block_env);

        (journaled_state.database, EvmEnv { block_env: blocks, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn components(&self) -> (&Self::DB, &Self::Inspector, &Self::Precompiles) {
        (&self.inner.ctx.journaled_state.database, &self.inner.inspector, &self.inner.precompiles)
    }

    fn components_mut(&mut self) -> (&mut Self::DB, &mut Self::Inspector, &mut Self::Precompiles) {
        (
            &mut self.inner.ctx.journaled_state.database,
            &mut self.inner.inspector,
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
        let mut evm = EthEvmBuilder::new(db, input.clone()).build();
        apply_multichain_overrides(&mut evm, &input);
        evm
    }

    fn create_evm_with_inspector<DB: MultiDatabase, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let mut evm = EthEvmBuilder::new(db, input.clone()).activate_inspector(inspector).build();
        apply_multichain_overrides(&mut evm, &input);
        evm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;
    use revm::{
        database::EmptyDB,
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
            let mut early_evm = factory.create_evm(EmptyDB::default(), early_env);

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
            let mut later_evm = factory.create_evm(EmptyDB::default(), later_env);

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
        cfg_env.spec = SpecId::CANCUN;

        let mut block_map = HashMap::default();
        block_map.insert(cfg_env.chain_id, BlockEnv::default());
        block_map.insert(1, BlockEnv::default());
        let expected_blocks = block_map.clone();

        let env = EvmEnv { block_env: block_map, cfg_env: cfg_env.clone() };

        let factory = EthEvmFactory;
        let evm = factory.create_evm(EmptyDB::default(), env.clone());
        let ctx = evm.ctx();

        let cfg = &ctx.cfg;
        assert_eq!(cfg.chain_id, env.cfg_env.chain_id);
        assert_eq!(cfg.spec, env.cfg_env.spec);

        for chain_id in expected_blocks.keys() {
            assert!(evm.blocks().contains_key(chain_id), "missing block env for chain {}", chain_id);
        }
    }

    #[test]
    fn inspector_factory_keeps_overrides() {
        let mut cfg_env = CfgEnv::default();
        cfg_env.chain_id = 200;
        cfg_env.spec = SpecId::BERLIN;

        let mut block_map = HashMap::default();
        block_map.insert(cfg_env.chain_id, BlockEnv::default());
        block_map.insert(1, BlockEnv::default());

        let env = EvmEnv { block_env: block_map, cfg_env: cfg_env.clone() };

        let factory = EthEvmFactory;
        let inspector = NoOpInspector {};
        let evm = factory.create_evm_with_inspector(EmptyDB::default(), env.clone(), inspector);
        let ctx = evm.ctx();

        assert_eq!(ctx.cfg.chain_id, env.cfg_env.chain_id);
        assert_eq!(ctx.cfg.spec, env.cfg_env.spec);
        assert!(evm.blocks().contains_key(&cfg_env.chain_id));
        assert!(evm.blocks().contains_key(&1));
    }
}
