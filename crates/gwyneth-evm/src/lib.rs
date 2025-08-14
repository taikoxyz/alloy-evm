#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/alloy-rs/core/main/assets/alloy.jpg",
    html_favicon_url = "https://raw.githubusercontent.com/alloy-rs/core/main/assets/favicon.ico"
)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::vec::Vec;
use alloy_evm::{precompiles::PrecompilesMap, Database, Evm, EvmEnv, EvmFactory};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use core::{
    fmt::Debug,
    ops::{Deref, DerefMut},
};

use revm::{
    context::{BlockEnv, TxEnv},
    handler::{instructions::EthInstructions},
    inspector::NoOpInspector,
    Context, ExecuteEvm, InspectEvm, Inspector,
    context_interface::result::{EVMError, HaltReason, ResultAndState},
        primitives::hardfork::SpecId,
};
use gwyneth_revm::{GwynethContext, GwynethTransaction, GwynethPrecompiles};



/// GwynethEvm based On  Evm
#[allow(missing_debug_implementations)]
pub struct GwynethEvm<DB: Database, INSP> {
    inner: gwyneth_revm::GwynethEvm<DB, INSP>,
    inspect: bool,
}

impl<DB: Database, INSP> GwynethEvm<DB, INSP> {
    /// Creates a new Gwyneth EVM instance.
    ///
    /// The `inspect` argument determines whether the configured [`Inspector`] of the given
    /// [`GwynethEvm`](gwyneth_revm::GwynethEvm) should be invoked on [`Evm::transact`].
    pub const fn new(
        evm: gwyneth_revm::GwynethEvm<DB, INSP>,
        inspect: bool,
    ) -> Self {
        Self { inner: evm, inspect }
    }

    /// Consumes self and return the inner [`GwynethEvm`](gwyneth_revm::GwynethEvm) instance.
    pub fn into_inner(
        self,
    ) -> gwyneth_revm::GwynethEvm<DB, INSP>
    {
        self.inner
    }

    /// Provides a reference to the [`GwynethEvm`](gwyneth_revm::GwynethEvm) context.
    pub const fn ctx(&self) -> &gwyneth_revm::GwynethContext<DB> {
        &self.inner.0.ctx
    }

    /// Provides a mutable reference to the [`GwynethEvm`](gwyneth_revm::GwynethEvm) context.
    pub fn ctx_mut(&mut self) -> &mut gwyneth_revm::GwynethContext<DB> {
        &mut self.inner.0.ctx
    }
}
impl<DB: Database, INSP> Deref for GwynethEvm<DB, INSP> {
    type Target = GwynethContext<DB>;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.ctx()
    }
}

impl<DB: Database, INSP> DerefMut for GwynethEvm<DB, INSP> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx_mut()
    }
}

/*

impl<DB, INSP> Evm for GwynethEvm<DB, INSP>
where
    DB: Database,
    INSP: Inspector<GwynethContext<DB>>,
{
    type DB = DB;
    type Tx = GwynethTransaction<TxEnv>;
    type Error = EVMError<DB::Error>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type Precompiles = GwynethPrecompiles;
    type Inspector = INSP;

    fn block(&self) -> &BlockEnv {
        &self.block
    }

    fn chain_id(&self) -> u64 {
        self.cfg.chain_id
    }

    fn transact_raw(
        &mut self,
        tx: Self::Tx,
    ) -> Result<ResultAndState<Self::HaltReason>, Self::Error> {
        if self.inspect {
            self.inner.set_tx(tx);
            self.inner.inspect_replay()
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
        let tx = TxEnv {
                caller,
                kind: TxKind::Call(contract),
                // Explicitly set nonce to 0 so revm does not do any nonce checks
                nonce: 0,
                gas_limit: 30_000_000,
                value: U256::ZERO,
                data,
                // Setting the gas price to zero enforces that no value is transferred as part of
                // the call, and that the call will not count against the block's
                // gas limit
                gas_price: 0,
                // The chain ID check is not relevant here and is disabled if set to None
                chain_id: None,
                // Setting the gas priority fee to None ensures the effective gas price is derived
                // from the `gas_price` field, which we need to be zero
                gas_priority_fee: None,
                access_list: Default::default(),
                // blob fields can be None for this tx
                blob_hashes: Vec::new(),
                max_fee_per_blob_gas: 0,
                tx_type: OpTxType::Deposit as u8,
                authorization_list: Default::default(),
            }.into();

        let mut gas_limit = tx.base.gas_limit;
        let mut basefee = 0;
        let mut disable_nonce_check = true;

        // ensure the block gas limit is >= the tx
        core::mem::swap(&mut self.block.gas_limit, &mut gas_limit);
        // disable the base fee check for this call by setting the base fee to zero
        core::mem::swap(&mut self.block.basefee, &mut basefee);
        // disable the nonce check
        core::mem::swap(&mut self.cfg.disable_nonce_check, &mut disable_nonce_check);

        let mut res = self.transact(tx);

        // swap back to the previous gas limit
        core::mem::swap(&mut self.block.gas_limit, &mut gas_limit);
        // swap back to the previous base fee
        core::mem::swap(&mut self.block.basefee, &mut basefee);
        // swap back to the previous nonce check flag
        core::mem::swap(&mut self.cfg.disable_nonce_check, &mut disable_nonce_check);

        // NOTE: We assume that only the contract storage is modified. Revm currently marks the
        // caller and block beneficiary accounts as "touched" when we do the above transact calls,
        // and includes them in the result.
        //
        // We're doing this state cleanup to make sure that changeset only includes the changed
        // contract storage.
        if let Ok(res) = &mut res {
            res.state.retain(|addr, _| *addr == contract);
        }

        res
    }

    fn db_mut(&mut self) -> &mut Self::DB {
        &mut self.journaled_state.database
    }

    fn finish(self) -> (Self::DB, EvmEnv<Self::Spec>) {
        let Context { block: block_env, cfg: cfg_env, journaled_state, .. } = self.inner.0.ctx;

        (journaled_state.database, EvmEnv { block_env, cfg_env })
    }

    fn set_inspector_enabled(&mut self, enabled: bool) {
        self.inspect = enabled;
    }

    fn precompiles(&self) -> &Self::Precompiles {
        &self.inner.0.precompiles
    }

    fn precompiles_mut(&mut self) -> &mut Self::Precompiles {
        &mut self.inner.0.precompiles
    }

    fn inspector(&self) -> &Self::Inspector {
        &self.inner.0.inspector
    }

    fn inspector_mut(&mut self) -> &mut Self::Inspector {
        &mut self.inner.0.inspector
    }
}

/// Factory producing [`OpEvm`]s.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct GwynethEvmFactory;

impl EvmFactory for GwynethEvmFactory {
    type Evm<DB: Database, INSP: Inspector<GwynethContext<DB>>> = GwynethEvm<DB, INSP>;
    type Context<DB: Database> = GwynethContext<DB>;
    type Tx = GwynethTransaction<TxEnv>;
    type Error<DBError: core::error::Error + Send + Sync + 'static> =
        EVMError<DBError>;
    type HaltReason = HaltReason;
    type Spec = SpecId;
    type Precompiles = GwynethPrecompiles;

    fn create_evm<DB: Database>(
        &self,
        db: DB,
        input: EvmEnv<SpecId>,
    ) -> Self::Evm<DB, NoOpInspector> {
        let spec_id = input.cfg_env.spec;
        GwynethEvm {
            inner: Context::op()
                .with_db(db)
                .with_block(input.block_env)
                .with_cfg(input.cfg_env)
                .build_with_inspector(NoOpInspector {})
                .with_precompiles(PrecompilesMap::from_static(
                    GwynethPrecompiles::new_with_spec(spec_id).precompiles(),
                )),
            inspect: false,
        }
    }

    fn create_evm_with_inspector<DB: Database, I: Inspector<Self::Context<DB>>>(
        &self,
        db: DB,
        input: EvmEnv<SpecId>,
        inspector: I,
    ) -> Self::Evm<DB, I> {
        let spec_id = input.cfg_env.spec;
        GwynethEvm {
            inner: Context::op()
                .with_db(db)
                .with_block(input.block_env)
                .with_cfg(input.cfg_env)
                .build_with_inspector(inspector)
                .with_precompiles(PrecompilesMap::from_static(
                    Precompiles::new_with_spec(spec_id).precompiles(),
                )),
            inspect: true,
        }
    }
}
*/