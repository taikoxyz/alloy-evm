//! Block executor for Optimism.

use crate::OpEvmFactory;
use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use alloy_consensus::{Eip658Value, Header, Transaction, TxReceipt};
use alloy_eips::{Encodable2718, Typed2718};
use alloy_evm::{
    block::{
        state_changes::{balance_increment_state, post_block_balance_increments},
        BlockExecutionError, BlockExecutionResult, BlockExecutor, BlockExecutorFactory,
        BlockExecutorFor, BlockValidationError, CommitChanges, ExecutableTx, OnStateHook,
        StateChangePostBlockSource, StateChangeSource, SystemCaller,
    },
    eth::receipt_builder::ReceiptBuilderCtx,
    Evm, EvmFactory, FromRecoveredTx, FromTxWithEncoded, MultiDatabase,
};
use alloy_op_hardforks::{OpChainHardforks, OpHardforks};
use alloy_primitives::{Bytes, B256};
use canyon::ensure_create2_deployer;
use op_alloy_consensus::OpDepositReceipt;
// Stub for DEPOSIT_TRANSACTION_TYPE to avoid op-revm compilation issues
const DEPOSIT_TRANSACTION_TYPE: u8 = 0x7E;
pub use receipt_builder::OpAlloyReceiptBuilder;
use receipt_builder::OpReceiptBuilder;
use revm::{
    context::result::{ExecutionResult, ResultAndState},
    database::State,
    database_interface::MultiChainDatabaseCommit,
    primitives::{ChainAddress, HashMap},
    Inspector,
};

mod canyon;
pub mod receipt_builder;

/// Context for OP block execution.
#[derive(Debug, Default, Clone)]
pub struct OpBlockExecutionCtx {
    /// Parent block hash.
    pub parent_hash: B256,
    /// Parent beacon block root.
    pub parent_beacon_block_root: Option<B256>,
    /// The block's extra data.
    pub extra_data: Bytes,
}

/// Block executor for Optimism.
#[derive(Debug)]
pub struct OpBlockExecutor<Evm, R: OpReceiptBuilder, Spec> {
    /// Spec.
    spec: Spec,
    /// Receipt builder.
    receipt_builder: R,

    /// Context for block execution.
    ctx: OpBlockExecutionCtx,
    /// The EVM used by executor.
    evm: Evm,
    /// Receipts of executed transactions.
    receipts: Vec<R::Receipt>,
    /// Total gas used by executed transactions.
    gas_used: u64,
    /// Whether Regolith hardfork is active.
    is_regolith: bool,
    /// Utility to call system smart contracts.
    system_caller: SystemCaller<Spec>,
}

impl<E, R, Spec> OpBlockExecutor<E, R, Spec>
where
    E: Evm,
    R: OpReceiptBuilder,
    Spec: OpHardforks + Clone,
{
    /// Creates a new [`OpBlockExecutor`].
    pub fn new(evm: E, ctx: OpBlockExecutionCtx, spec: Spec, receipt_builder: R) -> Self {
        Self {
            is_regolith: spec
                .is_regolith_active_at_timestamp(evm.block().timestamp.saturating_to()),
            evm,
            system_caller: SystemCaller::new(spec.clone()),
            spec,
            receipt_builder,
            receipts: Vec::new(),
            gas_used: 0,
            ctx,
        }
    }
}

impl<'db, DB, E, R, Spec> BlockExecutor for OpBlockExecutor<E, R, Spec>
where
    DB: MultiDatabase + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag =
            self.spec.is_spurious_dragon_active_at_block(self.evm.block().number.saturating_to());
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);

        let chain_id = self.evm.chain_id();
        self.system_caller.apply_blockhashes_contract_call(
            self.ctx.parent_hash,
            &mut self.evm,
            chain_id,
        )?;
        self.system_caller.apply_beacon_root_contract_call(
            self.ctx.parent_beacon_block_root,
            &mut self.evm,
            chain_id,
        )?;

        // Ensure that the create2deployer is force-deployed at the canyon transition. Optimism
        // blocks will always have at least a single transaction in them (the L1 info transaction),
        // so we can safely assume that this will always be triggered upon the transition and that
        // the above check for empty blocks will never be hit on OP chains.
        let chain_id = self.evm.chain_id();
        ensure_create2_deployer(
            &self.spec,
            self.evm.block().timestamp.saturating_to(),
            self.evm.db_mut(),
            chain_id,
        )
        .map_err(BlockExecutionError::other)?;

        Ok(())
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        let is_deposit = tx.tx().ty() == DEPOSIT_TRANSACTION_TYPE;

        // The sum of the transaction’s gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block’s gasLimit.
        let block_available_gas = self.evm.block().gas_limit - self.gas_used;
        if tx.tx().gas_limit() > block_available_gas && (self.is_regolith || !is_deposit) {
            return Err(BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: tx.tx().gas_limit(),
                block_available_gas,
            }
            .into());
        }

        // Cache the depositor account prior to the state transition for the deposit nonce.
        //
        // Note that this *only* needs to be done post-regolith hardfork, as deposit nonces
        // were not introduced in Bedrock. In addition, regular transactions don't have deposit
        // nonces, so we don't need to touch the DB for those.
        let depositor = (self.is_regolith && is_deposit)
            .then(|| {
                let chain_id = self.evm.chain_id();
                self.evm
                    .db_mut()
                    .load_cache_account(ChainAddress::new(chain_id, *tx.signer()))
                    .map(|acc| acc.account_info().unwrap_or_default())
            })
            .transpose()
            .map_err(BlockExecutionError::other)?;

        let hash = tx.tx().trie_hash();

        // Execute transaction.
        let ResultAndState { result, state } =
            self.evm.transact(&tx).map_err(move |err| BlockExecutionError::evm(err, hash))?;

        if !f(&result).should_commit() {
            return Ok(None);
        }

        self.system_caller.on_state(StateChangeSource::Transaction(self.receipts.len()), &state);

        let gas_used = result.gas_used();

        // append gas used
        self.gas_used += gas_used;

        self.receipts.push(
            match self.receipt_builder.build_receipt(ReceiptBuilderCtx {
                tx: tx.tx(),
                result,
                cumulative_gas_used: self.gas_used,
                evm: &self.evm,
                state: &state,
            }) {
                Ok(receipt) => receipt,
                Err(ctx) => {
                    let receipt = alloy_consensus::Receipt {
                        // Success flag was added in `EIP-658: Embedding transaction status code
                        // in receipts`.
                        status: Eip658Value::Eip658(ctx.result.is_success()),
                        cumulative_gas_used: self.gas_used,
                        logs: ctx.result.into_logs(),
                    };

                    self.receipt_builder.build_deposit_receipt(OpDepositReceipt {
                        inner: receipt,
                        deposit_nonce: depositor.map(|account| account.nonce),
                        // The deposit receipt version was introduced in Canyon to indicate an
                        // update to how receipt hashes should be computed
                        // when set. The state transition process ensures
                        // this is only set for post-Canyon deposit
                        // transactions.
                        deposit_receipt_version: (is_deposit
                            && self.spec.is_canyon_active_at_timestamp(
                                self.evm.block().timestamp.saturating_to(),
                            ))
                        .then_some(1),
                    })
                }
            },
        );

        self.evm.db_mut().commit_multi(state);

        Ok(Some(gas_used))
    }

    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        let balance_increments =
            post_block_balance_increments::<Header>(&self.spec, self.evm.block(), &[], None);
        // increment balances
        let chain_id = self.evm.chain_id();
        self.evm
            .db_mut()
            .increment_balances(
                balance_increments
                    .iter()
                    .map(|(addr, balance)| (ChainAddress::new(chain_id, *addr), *balance)),
            )
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;
        // call state hook with changes due to balance increments.
        let chain_id = self.evm.chain_id();
        self.system_caller.try_on_state_with(|| {
            balance_increment_state(&balance_increments, self.evm.db_mut(), chain_id).map(|state| {
                (
                    StateChangeSource::PostBlock(StateChangePostBlockSource::BalanceIncrements),
                    Cow::Owned(state),
                )
            })
        })?;

        let gas_used = self.receipts.last().map(|r| r.cumulative_gas_used()).unwrap_or_default();
        Ok((
            self.evm,
            BlockExecutionResult {
                receipts: self.receipts,
                requests: Default::default(),
                gas_used,
                gwyneth_journal: Vec::new(),
                gas_used_per_chain: Default::default(),
            },
        ))
    }

    fn set_state_hook(&mut self, hook: Option<Box<dyn OnStateHook>>) {
        self.system_caller.with_state_hook(hook);
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }

    fn evm(&self) -> &Self::Evm {
        &self.evm
    }
}

/// Ethereum block executor factory.
#[derive(Debug, Clone, Default, Copy)]
pub struct OpBlockExecutorFactory<
    R = OpAlloyReceiptBuilder,
    Spec = OpChainHardforks,
    EvmFactory = OpEvmFactory,
> {
    /// Receipt builder.
    receipt_builder: R,
    /// Chain specification.
    spec: Spec,
    /// EVM factory.
    evm_factory: EvmFactory,
}

impl<R, Spec, EvmFactory> OpBlockExecutorFactory<R, Spec, EvmFactory> {
    /// Creates a new [`OpBlockExecutorFactory`] with the given spec, [`EvmFactory`], and
    /// [`OpReceiptBuilder`].
    pub const fn new(receipt_builder: R, spec: Spec, evm_factory: EvmFactory) -> Self {
        Self { receipt_builder, spec, evm_factory }
    }

    /// Exposes the receipt builder.
    pub const fn receipt_builder(&self) -> &R {
        &self.receipt_builder
    }

    /// Exposes the chain specification.
    pub const fn spec(&self) -> &Spec {
        &self.spec
    }

    /// Exposes the EVM factory.
    pub const fn evm_factory(&self) -> &EvmFactory {
        &self.evm_factory
    }
}

impl<R, Spec, EvmF> BlockExecutorFactory for OpBlockExecutorFactory<R, Spec, EvmF>
where
    R: OpReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt>,
    Spec: OpHardforks,
    EvmF: EvmFactory<Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>>,
    Self: 'static,
{
    type EvmFactory = EvmF;
    type ExecutionCtx<'a> = OpBlockExecutionCtx;
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EvmF::Evm<&'a mut State<DB>, I>,
        ctx: HashMap<u64, Self::ExecutionCtx<'a>>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: MultiDatabase + 'a,
        I: Inspector<EvmF::Context<&'a mut State<DB>>> + 'a,
    {
        // For OP, we take the single context (OP is single-chain)
        let single_ctx = ctx.into_values().next().unwrap_or_default();
        OpBlockExecutor::new(evm, single_ctx, &self.spec, &self.receipt_builder)
    }
}

#[cfg(test)]
mod tests {
    // Imports commented out since op-revm is stubbed
    // use alloy_consensus::{transaction::Recovered, SignableTransaction, TxLegacy};
    // use alloy_eips::eip2718::WithEncoded;
    // use alloy_evm::EvmEnv;
    // use alloy_primitives::{Address, Signature};
    // use op_alloy_consensus::OpTxEnvelope;
    // use revm::database::MultiEmptyDB;
    // use super::*;

    #[test]
    #[ignore = "OpEvm is a stub implementation - Op code won't be used"]
    fn test_with_encoded() {
        // Test body commented out since op-revm is stubbed and won't compile
        // The test is preserved for when op-revm is properly integrated
        // let executor_factory = OpBlockExecutorFactory::new(
        //     OpAlloyReceiptBuilder::default(),
        //     OpChainHardforks::op_mainnet(),
        //     OpEvmFactory::default(),
        // );
        // let mut db = State::builder().with_database(MultiEmptyDB::default()).build();
        // let evm = executor_factory.evm_factory.create_evm(&mut db, EvmEnv::default());
        // let mut executor = executor_factory.create_executor(evm, OpBlockExecutionCtx::default());
        // let tx = Recovered::new_unchecked(
        //     OpTxEnvelope::Legacy(TxLegacy::default().into_signed(Signature::new(
        //         Default::default(),
        //         Default::default(),
        //         Default::default(),
        //     ))),
        //     Address::ZERO,
        // );
        // let tx_with_encoded = WithEncoded::new(tx.encoded_2718().into(), tx.clone());

        // // make sure we can use both `WithEncoded` and transaction itself as inputs.
        // let _ = executor.execute_transaction(&tx);
        // let _ = executor.execute_transaction(&tx_with_encoded);
    }
}
