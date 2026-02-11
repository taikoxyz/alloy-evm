//! Hard-failure normalization contract tests for `alloy-gwyneth-evm`.

use alloy_evm::evm::BoundedEvmFactory as _;
use alloy_evm::Evm as _;
use alloy_gwyneth_evm::{GwynethEvmFactoryImpl, GwynethHaltReason};
use gwyneth_engine::HardFailureCode;
use gwyneth_phase64_shared_dev::{
    code_structural_violation, xcalloptions_word, HARD_FAILURE_BAD_TO, HARD_FAILURE_BENEFICIARY,
    HARD_FAILURE_BLOCK_GAS_LIMIT, HARD_FAILURE_CALLER, HARD_FAILURE_CALLER_BALANCE,
    HARD_FAILURE_CHAIN_IDS, HARD_FAILURE_ENTRY, HARD_FAILURE_TARGET, HARD_FAILURE_TX_GAS_LIMIT,
};
use gwyneth_types::ExecutionSurface;
use revm::{
    context_interface::result::ExecutionResult,
    database::InMemoryDB,
    primitives::{Address, Bytes, U256},
};

#[test]
fn redesign_alloy_smoke_hard_failure_normalization_transact_raw() {
    let caller = Address::from(HARD_FAILURE_CALLER);
    let entry = Address::from(HARD_FAILURE_ENTRY);
    let bad_to = Address::from(HARD_FAILURE_BAD_TO);
    let target = Address::from(HARD_FAILURE_TARGET);

    let mut l1 = InMemoryDB::default();
    let l2 = InMemoryDB::default();

    gwyneth_phase64_shared_dev::gwyneth_test_insert_eoa!(
        l1,
        caller,
        HARD_FAILURE_CALLER_BALANCE
    );
    gwyneth_phase64_shared_dev::gwyneth_test_insert_code!(
        l1,
        entry,
        code_structural_violation(xcalloptions_word(HARD_FAILURE_CHAIN_IDS[1], target, false), bad_to),
    );

    let db = gwyneth_phase64_shared_dev::gwyneth_test_overlay_db_single_l2!(
        HARD_FAILURE_CHAIN_IDS[0],
        l1,
        HARD_FAILURE_CHAIN_IDS[1],
        l2
    );

    let beneficiary = Address::from(HARD_FAILURE_BENEFICIARY);
    let (block_env, cfg_env) = gwyneth_phase64_shared_dev::gwyneth_test_base_env_cancun!(
        HARD_FAILURE_CHAIN_IDS[0],
        beneficiary,
        HARD_FAILURE_BLOCK_GAS_LIMIT
    );

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory
        .for_surface(ExecutionSurface::TxSubmission)
        .create_evm(db, alloy_evm::EvmEnv { block_env, cfg_env });

    let tx = gwyneth_phase64_shared_dev::gwyneth_test_tx_env_call!(
        HARD_FAILURE_CHAIN_IDS[0],
        caller,
        entry,
        HARD_FAILURE_TX_GAS_LIMIT,
        0,
        Some(0),
        U256::ZERO,
        Bytes::new(),
        0
    );

    let out = evm.transact_raw(tx).expect("hard failure is normalized");

    assert!(!out.result.is_success());
    assert_eq!(out.result.gas_used(), HARD_FAILURE_TX_GAS_LIMIT);
    assert!(out.result.logs().is_empty());
    assert!(out.result.output().is_none());

    match out.result {
        ExecutionResult::Halt { reason, gas_used } => {
            assert_eq!(gas_used, HARD_FAILURE_TX_GAS_LIMIT);

            match reason {
                GwynethHaltReason::GwynethHardFailure(hf) => {
                    assert_eq!(hf.chain_id, HARD_FAILURE_CHAIN_IDS[0]);
                    assert_eq!(hf.opcode, Some(revm::state::bytecode::opcode::CALL));
                    assert_eq!(
                        hf.code,
                        HardFailureCode::XcalloptionsMustBeFollowedByCallToExtensionOracle
                    );
                    assert_eq!(
                        hf.reason,
                        HardFailureCode::XcalloptionsMustBeFollowedByCallToExtensionOracle.reason()
                    );
                    assert_eq!(hf.gas_used, HARD_FAILURE_TX_GAS_LIMIT);
                    assert!(hf.logs.is_empty());
                    assert!(hf.output.is_empty());
                }
                other => panic!("unexpected halt reason: {other:?}"),
            }
        }
        other => panic!("expected halt: {other:?}"),
    }
}

#[test]
fn redesign_alloy_smoke_hard_failure_normalization_transact_system_call() {
    let caller = Address::from(HARD_FAILURE_CALLER);
    let entry = Address::from(HARD_FAILURE_ENTRY);
    let bad_to = Address::from(HARD_FAILURE_BAD_TO);
    let target = Address::from(HARD_FAILURE_TARGET);

    let mut l1 = InMemoryDB::default();
    let l2 = InMemoryDB::default();

    gwyneth_phase64_shared_dev::gwyneth_test_insert_eoa!(
        l1,
        caller,
        HARD_FAILURE_CALLER_BALANCE
    );
    gwyneth_phase64_shared_dev::gwyneth_test_insert_code!(
        l1,
        entry,
        code_structural_violation(xcalloptions_word(HARD_FAILURE_CHAIN_IDS[1], target, false), bad_to),
    );

    let db = gwyneth_phase64_shared_dev::gwyneth_test_overlay_db_single_l2!(
        HARD_FAILURE_CHAIN_IDS[0],
        l1,
        HARD_FAILURE_CHAIN_IDS[1],
        l2
    );

    let beneficiary = Address::from(HARD_FAILURE_BENEFICIARY);
    let (block_env, cfg_env) = gwyneth_phase64_shared_dev::gwyneth_test_base_env_cancun!(
        HARD_FAILURE_CHAIN_IDS[0],
        beneficiary,
        HARD_FAILURE_BLOCK_GAS_LIMIT
    );

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory
        .for_surface(ExecutionSurface::TxSubmission)
        .create_evm(db, alloy_evm::EvmEnv { block_env, cfg_env });

    // Exercise the inspector-enabled path (the gwyneth journal inspector stays active either way).
    evm.set_inspector_enabled(true);

    let out = evm
        .transact_system_call(
            caller,
            entry,
            Bytes::new(),
        )
        .expect("hard failure is normalized");

    assert!(!out.result.is_success());
    assert_eq!(out.result.gas_used(), HARD_FAILURE_BLOCK_GAS_LIMIT);
    assert!(out.result.logs().is_empty());
    assert!(out.result.output().is_none());

    match out.result {
        ExecutionResult::Halt { reason, gas_used } => {
            assert_eq!(gas_used, HARD_FAILURE_BLOCK_GAS_LIMIT);

            match reason {
                GwynethHaltReason::GwynethHardFailure(hf) => {
                    assert_eq!(hf.chain_id, HARD_FAILURE_CHAIN_IDS[0]);
                    assert_eq!(hf.opcode, Some(revm::state::bytecode::opcode::CALL));
                    assert_eq!(
                        hf.code,
                        HardFailureCode::XcalloptionsMustBeFollowedByCallToExtensionOracle
                    );
                    assert_eq!(
                        hf.reason,
                        HardFailureCode::XcalloptionsMustBeFollowedByCallToExtensionOracle.reason()
                    );
                    assert_eq!(hf.gas_used, HARD_FAILURE_BLOCK_GAS_LIMIT);
                    assert!(hf.logs.is_empty());
                    assert!(hf.output.is_empty());
                }
                other => panic!("unexpected halt reason: {other:?}"),
            }
        }
        other => panic!("expected halt: {other:?}"),
    }
}

#[test]
fn audit_user_inspector_disable_does_not_disable_gwyneth_inspector() {
    let caller = Address::from(HARD_FAILURE_CALLER);
    let entry = Address::from(HARD_FAILURE_ENTRY);
    let bad_to = Address::from(HARD_FAILURE_BAD_TO);
    let target = Address::from(HARD_FAILURE_TARGET);

    let mut l1 = InMemoryDB::default();
    let l2 = InMemoryDB::default();

    gwyneth_phase64_shared_dev::gwyneth_test_insert_eoa!(
        l1,
        caller,
        HARD_FAILURE_CALLER_BALANCE
    );
    gwyneth_phase64_shared_dev::gwyneth_test_insert_code!(
        l1,
        entry,
        code_structural_violation(xcalloptions_word(HARD_FAILURE_CHAIN_IDS[1], target, false), bad_to),
    );

    let db = gwyneth_phase64_shared_dev::gwyneth_test_overlay_db_single_l2!(
        HARD_FAILURE_CHAIN_IDS[0],
        l1,
        HARD_FAILURE_CHAIN_IDS[1],
        l2
    );

    let beneficiary = Address::from(HARD_FAILURE_BENEFICIARY);
    let (block_env, cfg_env) = gwyneth_phase64_shared_dev::gwyneth_test_base_env_cancun!(
        HARD_FAILURE_CHAIN_IDS[0],
        beneficiary,
        HARD_FAILURE_BLOCK_GAS_LIMIT
    );

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory
        .for_surface(ExecutionSurface::TxSubmission)
        .create_evm(db, alloy_evm::EvmEnv { block_env, cfg_env });

    // Disabling the user inspector must not disable the always-on `JournalInspector` that enforces
    // structural invariants and reports hard-failure details.
    evm.set_inspector_enabled(false);

    let tx = gwyneth_phase64_shared_dev::gwyneth_test_tx_env_call!(
        HARD_FAILURE_CHAIN_IDS[0],
        caller,
        entry,
        HARD_FAILURE_TX_GAS_LIMIT,
        0,
        Some(0),
        U256::ZERO,
        Bytes::new(),
        0
    );

    let out = evm.transact_raw(tx).expect("hard failure is normalized");
    assert!(!out.result.is_success());
}
