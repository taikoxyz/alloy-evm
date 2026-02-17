use super::*;
use crate::eth::ComparisonInputs;
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
        Vec::<
            Vec<(
                Address,
                Option<Option<AccountInfo>>,
                Vec<(revm::primitives::StorageKey, revm::primitives::StorageValue)>,
            )>,
        >::new(),
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

