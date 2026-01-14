//! Ethereum block executor.

use super::{
    dao_fork, eip6110,
    receipt_builder::{AlloyReceiptBuilder, ReceiptBuilder, ReceiptBuilderCtx},
    spec::{EthExecutorSpec, EthSpec},
    EthEvmFactory,
    PrecomputedBlockOutcome,
};
use crate::{
    block::{
        state_changes::{balance_increment_state, post_block_balance_increments},
        BlockExecutionError, BlockExecutionResult, BlockExecutor, BlockExecutorFactory,
        BlockExecutorFor, BlockValidationError, ExecutableTx, OnStateHook,
        StateChangePostBlockSource, StateChangeSource, SystemCaller,
    },
    Database, Evm, EvmFactory, FromRecoveredTx, FromTxWithEncoded,
};
use alloc::{borrow::Cow, boxed::Box, vec::Vec};
use alloy_consensus::{Header, ReceiptEnvelope, Transaction, TxReceipt};
use alloy_eips::{eip4895::Withdrawals, eip7685::Requests, Encodable2718};
use alloy_hardforks::EthereumHardfork;
use alloy_primitives::{Log, B256};
use core::ops::DerefMut;
use revm::{
    context::Block, context_interface::result::ResultAndState, database::State, DatabaseCommit,
    Inspector,
};

/// Context for Ethereum block execution.
#[derive(Debug, Clone)]
pub struct EthBlockExecutionCtx<'a, Receipt = ReceiptEnvelope> {
    /// Parent block hash.
    pub parent_hash: B256,
    /// Parent beacon block root.
    pub parent_beacon_block_root: Option<B256>,
    /// The block's extra data.
    pub extra_data: &'a [u8],
    /// Optional precomputed outcome for diff-backed execution.
    pub precomputed_outcome: Option<PrecomputedBlockOutcome<Receipt>>,
    /// Block ommers
    pub ommers: &'a [Header],
    /// Block withdrawals.
    pub withdrawals: Option<Cow<'a, Withdrawals>>,
}

/// Block executor for Ethereum.
#[derive(Debug)]
pub struct EthBlockExecutor<'a, Evm, Spec, R: ReceiptBuilder> {
    /// Reference to the specification object.
    pub spec: Spec,

    /// Context for block execution.
    pub ctx: EthBlockExecutionCtx<'a, R::Receipt>,
    /// Inner EVM.
    pub evm: Evm,
    /// Utility to call system smart contracts.
    pub system_caller: SystemCaller<Spec>,
    /// Receipt builder.
    pub receipt_builder: R,

    /// Receipts of executed transactions.
    pub receipts: Vec<R::Receipt>,
    /// Total gas used by transactions in this block.
    pub gas_used: u64,

    /// Blob gas used by the block.
    /// Before cancun activation, this is always 0.
    pub blob_gas_used: u64,
}

impl<'a, Evm, Spec, R> EthBlockExecutor<'a, Evm, Spec, R>
where
    Spec: Clone,
    R: ReceiptBuilder,
{
    /// Creates a new [`EthBlockExecutor`]
    pub fn new(
        evm: Evm,
        ctx: EthBlockExecutionCtx<'a, R::Receipt>,
        spec: Spec,
        receipt_builder: R,
    ) -> Self {
        Self {
            evm,
            ctx,
            receipts: Vec::new(),
            gas_used: 0,
            blob_gas_used: 0,
            system_caller: SystemCaller::new(spec.clone()),
            spec,
            receipt_builder,
        }
    }
}

impl<'db, DB, E, Spec, R> BlockExecutor for EthBlockExecutor<'_, E, Spec, R>
where
    DB: Database + 'db,
    E: Evm<
        DB = &'db mut State<DB>,
        Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>,
    >,
    Spec: EthExecutorSpec,
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type Evm = E;

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        if self.ctx.precomputed_outcome.is_some() {
            return Ok(());
        }

        // Set state clear flag if the block is after the Spurious Dragon hardfork.
        let state_clear_flag =
            self.spec.is_spurious_dragon_active_at_block(self.evm.block().number().saturating_to());
        self.evm.db_mut().set_state_clear_flag(state_clear_flag);

        self.system_caller.apply_blockhashes_contract_call(self.ctx.parent_hash, &mut self.evm)?;
        self.system_caller
            .apply_beacon_root_contract_call(self.ctx.parent_beacon_block_root, &mut self.evm)?;

        Ok(())
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&revm::context::result::ExecutionResult<<Self::Evm as Evm>::HaltReason>) -> crate::block::CommitChanges,
    ) -> Result<Option<u64>, BlockExecutionError> {
        if self.ctx.precomputed_outcome.is_some() {
            return Ok(None);
        }

        // Execute transaction without committing.
        let output = self.execute_transaction_without_commit(&tx)?;

        if !f(&output.result).should_commit() {
            return Ok(None);
        }

        let gas_used = self.commit_transaction(output, tx)?;
        Ok(Some(gas_used))
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<ResultAndState<<Self::Evm as Evm>::HaltReason>, BlockExecutionError> {
        // The sum of the transaction's gas limit, Tg, and the gas utilized in this block prior,
        // must be no greater than the block's gasLimit.
        let block_available_gas = self.evm.block().gas_limit() - self.gas_used;

        if tx.tx().gas_limit() > block_available_gas {
            return Err(BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit: tx.tx().gas_limit(),
                block_available_gas,
            }
            .into());
        }

        // Execute transaction and return the result
        self.evm.transact(&tx).map_err(|err| {
            let hash = tx.tx().trie_hash();
            BlockExecutionError::evm(err, hash)
        })
    }

    fn commit_transaction(
        &mut self,
        output: ResultAndState<<Self::Evm as Evm>::HaltReason>,
        tx: impl ExecutableTx<Self>,
    ) -> Result<u64, BlockExecutionError> {
        let ResultAndState { result, state } = output;

        self.system_caller.on_state(StateChangeSource::Transaction(self.receipts.len()), &state);

        let gas_used = result.gas_used();

        // append gas used
        self.gas_used += gas_used;

        // only determine cancun fields when active
        if self.spec.is_cancun_active_at_timestamp(self.evm.block().timestamp().saturating_to()) {
            let tx_blob_gas_used = tx.tx().blob_gas_used().unwrap_or_default();

            self.blob_gas_used = self.blob_gas_used.saturating_add(tx_blob_gas_used);
        }

        // Push transaction changeset and calculate header bloom filter for receipt.
        self.receipts.push(self.receipt_builder.build_receipt(ReceiptBuilderCtx {
            tx: tx.tx(),
            evm: &self.evm,
            result,
            state: &state,
            cumulative_gas_used: self.gas_used,
        }));

        // Commit the state changes.
        self.evm.db_mut().commit(state);

        Ok(gas_used)
    }

    fn finish(
        mut self,
    ) -> Result<(Self::Evm, BlockExecutionResult<R::Receipt>), BlockExecutionError> {
        if let Some(precomputed) = self.ctx.precomputed_outcome {
            // Install the diff bundle directly into the underlying State and prevent any
            // additional transition merges from mutating it.
            let state = self.evm.db_mut().deref_mut();
            state.transition_state = None;
            state.bundle_state = precomputed.bundle;

            return Ok((self.evm, precomputed.result));
        }

        let requests = if self
            .spec
            .is_prague_active_at_timestamp(self.evm.block().timestamp().saturating_to())
        {
            // Collect all EIP-6110 deposits
            let deposit_requests =
                eip6110::parse_deposits_from_receipts(&self.spec, &self.receipts)?;

            let mut requests = Requests::default();

            if !deposit_requests.is_empty() {
                requests.push_request_with_type(eip6110::DEPOSIT_REQUEST_TYPE, deposit_requests);
            }

            requests.extend(self.system_caller.apply_post_execution_changes(&mut self.evm)?);
            requests
        } else {
            Requests::default()
        };

        let mut balance_increments = post_block_balance_increments(
            &self.spec,
            self.evm.block(),
            self.ctx.ommers,
            self.ctx.withdrawals.as_deref(),
        );

        // Irregular state change at Ethereum DAO hardfork
        if self
            .spec
            .ethereum_fork_activation(EthereumHardfork::Dao)
            .transitions_at_block(self.evm.block().number().saturating_to())
        {
            // drain balances from hardcoded addresses.
            let drained_balance: u128 = self
                .evm
                .db_mut()
                .drain_balances(dao_fork::DAO_HARDFORK_ACCOUNTS)
                .map_err(|_| BlockValidationError::IncrementBalanceFailed)?
                .into_iter()
                .sum();

            // return balance to DAO beneficiary.
            *balance_increments.entry(dao_fork::DAO_HARDFORK_BENEFICIARY).or_default() +=
                drained_balance;
        }
        // increment balances
        self.evm
            .db_mut()
            .increment_balances(balance_increments.clone())
            .map_err(|_| BlockValidationError::IncrementBalanceFailed)?;

        // call state hook with changes due to balance increments.
        self.system_caller.try_on_state_with(|| {
            balance_increment_state(&balance_increments, self.evm.db_mut()).map(|state| {
                (
                    StateChangeSource::PostBlock(StateChangePostBlockSource::BalanceIncrements),
                    Cow::Owned(state),
                )
            })
        })?;

        Ok((
            self.evm,
            BlockExecutionResult {
                receipts: self.receipts,
                requests,
                gas_used: self.gas_used,
                blob_gas_used: self.blob_gas_used,
            },
        ))
    }

    fn has_precomputed_outcome(&self) -> bool {
        self.ctx.precomputed_outcome.is_some()
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
pub struct EthBlockExecutorFactory<
    R = AlloyReceiptBuilder,
    Spec = EthSpec,
    EvmFactory = EthEvmFactory,
> {
    /// Receipt builder.
    receipt_builder: R,
    /// Chain specification.
    spec: Spec,
    /// EVM factory.
    evm_factory: EvmFactory,
}

impl<R, Spec, EvmFactory> EthBlockExecutorFactory<R, Spec, EvmFactory> {
    /// Creates a new [`EthBlockExecutorFactory`] with the given spec, [`EvmFactory`], and
    /// [`ReceiptBuilder`].
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

impl<R, Spec, EvmF> BlockExecutorFactory for EthBlockExecutorFactory<R, Spec, EvmF>
where
    R: ReceiptBuilder<Transaction: Transaction + Encodable2718, Receipt: TxReceipt<Log = Log>>,
    Spec: EthExecutorSpec,
    EvmF: EvmFactory<Tx: FromRecoveredTx<R::Transaction> + FromTxWithEncoded<R::Transaction>>,
    Self: 'static,
{
    type EvmFactory = EvmF;
    type ExecutionCtx<'a> = EthBlockExecutionCtx<'a, R::Receipt>;
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;

    fn evm_factory(&self) -> &Self::EvmFactory {
        &self.evm_factory
    }

    fn create_executor<'a, DB, I>(
        &'a self,
        evm: EvmF::Evm<&'a mut State<DB>, I>,
        ctx: Self::ExecutionCtx<'a>,
    ) -> impl BlockExecutorFor<'a, Self, DB, I>
    where
        DB: Database + 'a,
        I: Inspector<EvmF::Context<&'a mut State<DB>>> + 'a,
    {
        EthBlockExecutor::new(evm, ctx, &self.spec, &self.receipt_builder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::{ComparisonInputs, ParentHeaderView};
    use alloy_consensus::{transaction::Recovered, Signed, TxEnvelope, TxLegacy};
    use alloy_primitives::map::HashMap;
    use alloy_primitives::{Address, Bloom, Bytes, Signature, U256};
    use revm::{
        database::{
            states::{bundle_state::BundleRetention, BundleState},
            State as RevmState,
        },
        primitives::B256 as RevmB256,
        state::{AccountInfo, Bytecode},
    };

    #[derive(Debug, Default)]
    struct PanicDb;

    impl revm::Database for PanicDb {
        type Error = core::convert::Infallible;

        fn basic(&mut self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
            panic!("unexpected DB access")
        }

        fn code_by_hash(&mut self, _code_hash: RevmB256) -> Result<Bytecode, Self::Error> {
            panic!("unexpected DB access")
        }

        fn storage(
            &mut self,
            _address: Address,
            _index: revm::primitives::StorageKey,
        ) -> Result<revm::primitives::StorageValue, Self::Error> {
            panic!("unexpected DB access")
        }

        fn block_hash(&mut self, _number: u64) -> Result<RevmB256, Self::Error> {
            panic!("unexpected DB access")
        }
    }

    fn dummy_tx() -> Recovered<TxEnvelope> {
        let tx = TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 0,
            gas_limit: 21_000,
            to: alloy_primitives::TxKind::Call(Address::ZERO),
            value: U256::ZERO,
            input: Bytes::new(),
        };

        let mut sig_bytes = [0u8; 65];
        sig_bytes[64] = 27;
        let sig = Signature::from_raw_array(&sig_bytes).expect("signature bytes");

        let signed = Signed::new_unchecked(tx, sig, B256::ZERO);
        let envelope = TxEnvelope::Legacy(signed);
        Recovered::new_unchecked(envelope, Address::ZERO)
    }

    fn non_empty_bundle() -> BundleState {
        BundleState::new(
            [(
                Address::from([0x11; 20]),
                None,
                Some(AccountInfo::default()),
                HashMap::default(),
            )],
            Vec::<Vec<(Address, Option<Option<AccountInfo>>, Vec<(revm::primitives::StorageKey, revm::primitives::StorageValue)>)>>::new(),
            Vec::<(RevmB256, Bytecode)>::new(),
        )
    }

    #[test]
    fn precomputed_outcome_short_circuits_execution_and_installs_bundle() {
        let mut state = RevmState::builder()
            .with_database(PanicDb::default())
            .with_bundle_update()
            .without_state_clear()
            .build();

        let mut cfg_env = revm::context::CfgEnv::default();
        cfg_env.spec = revm::primitives::hardfork::SpecId::CANCUN;
        cfg_env.chain_id = 1;

        let mut block_env = revm::context::BlockEnv::default();
        block_env.number = U256::from(1);
        block_env.timestamp = U256::from(25);
        block_env.gas_limit = 30_000_000;

        let evm_env = crate::EvmEnv { block_env, cfg_env };
        let evm = EthEvmFactory::default().create_evm(&mut state, evm_env);

        let bundle = non_empty_bundle();
        let expected = BlockExecutionResult::<ReceiptEnvelope> {
            receipts: Vec::new(),
            requests: Requests::default(),
            gas_used: 123,
            blob_gas_used: 0,
        };

        let precomputed = PrecomputedBlockOutcome {
            result: expected.clone(),
            bundle,
            comparison_inputs: ComparisonInputs {
                tx_root: B256::ZERO,
                receipts_root: B256::ZERO,
                logs_bloom: Bloom::ZERO,
                gas_used: expected.gas_used,
                withdrawals_root: None,
                blob_gas_used: None,
                requests_hash: None,
            },
            parent_header: ParentHeaderView { timestamp: 0, blob_gas_used: None, excess_blob_gas: None },
        };

        let ctx = EthBlockExecutionCtx {
            parent_hash: B256::ZERO,
            parent_beacon_block_root: None,
            extra_data: &[],
            precomputed_outcome: Some(precomputed),
            ommers: &[],
            withdrawals: None,
        };

        let mut executor = EthBlockExecutor::new(evm, ctx, EthSpec::mainnet(), AlloyReceiptBuilder);

        executor
            .apply_pre_execution_changes()
            .expect("precomputed path skips system calls");

        executor
            .execute_transaction(&dummy_tx())
            .expect("precomputed path skips tx execution");

        let (mut evm, result) = executor.finish().expect("finish succeeds");
        assert_eq!(result, expected);

        let state = evm.db_mut().deref_mut();
        assert!(state.transition_state.is_none());
        assert_eq!(state.bundle_state.state.len(), 1);

        state.merge_transitions(BundleRetention::Reverts);
        assert_eq!(state.bundle_state.state.len(), 1);
    }
}
