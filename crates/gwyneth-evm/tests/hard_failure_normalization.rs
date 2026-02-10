//! Hard-failure normalization contract tests for `alloy-gwyneth-evm`.

use alloy_evm::evm::BoundedEvmFactory as _;
use alloy_evm::Evm as _;
use alloy_gwyneth_evm::{GwynethEvmFactoryImpl, GwynethHaltReason};
use gwyneth_engine::{HardFailureCode, L2OverlayDb};
use gwyneth_phase64_shared_dev::{code_structural_violation, xcalloptions_word};
use gwyneth_types::ExecutionSurface;
use revm::{
    context::{block::BlockEnv, cfg::CfgEnv, tx::TxEnv},
    database::InMemoryDB,
    context_interface::result::ExecutionResult,
    primitives::{Address, Bytes, TxKind, U256},
    state::{AccountInfo, Bytecode},
};

fn insert_code(db: &mut InMemoryDB, addr: Address, code: Vec<u8>) {
    let info = AccountInfo::default().with_code(Bytecode::new_raw(Bytes::from(code)));
    db.insert_account_info(addr, info);
}

fn insert_eoa(db: &mut InMemoryDB, addr: Address) {
    let info = AccountInfo::default().with_balance(U256::from(1_000_000u64));
    db.insert_account_info(addr, info);
}

fn base_env() -> (BlockEnv, CfgEnv) {
    let mut cfg_env = CfgEnv::default();
    cfg_env.spec = revm::primitives::hardfork::SpecId::CANCUN;
    cfg_env.chain_id = 1;

    let mut block_env = BlockEnv::default();
    block_env.beneficiary = Address::ZERO;
    block_env.gas_limit = 30_000_000;
    (block_env, cfg_env)
}

#[test]
fn redesign_alloy_smoke_hard_failure_normalization_transact_raw() {
    let caller = Address::from([0x10; 20]);
    let entry = Address::from([0x13; 20]);
    let bad_to = Address::from([0x99; 20]);
    let target = Address::from([0x22; 20]);

    let mut l1 = InMemoryDB::default();
    let l2 = InMemoryDB::default();

    insert_eoa(&mut l1, caller);
    insert_code(
        &mut l1,
        entry,
        code_structural_violation(xcalloptions_word(2, target, false), bad_to),
    );

    let mut db = gwyneth_engine::build_l2_overlay_db_adapter(1, l1, std::iter::empty(), 1).expect("overlay db init must succeed");
    db.l2_overlays.insert(2, l2);

    let (block_env, cfg_env) = base_env();

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory
        .for_surface(ExecutionSurface::TxSubmission)
        .create_evm(db, alloy_evm::EvmEnv { block_env, cfg_env });

    let tx = TxEnv::builder()
        .chain_id(Some(1))
        .caller(caller)
        .kind(TxKind::Call(entry))
        .gas_limit(123_456)
        .gas_price(0)
        .value(U256::ZERO)
        .data(Bytes::new())
        .build()
        .expect("tx build");

    let out = evm.transact_raw(tx).expect("hard failure is normalized");

    assert!(!out.result.is_success());
    assert_eq!(out.result.gas_used(), 123_456);
    assert!(out.result.logs().is_empty());
    assert!(out.result.output().is_none());

    match out.result {
        ExecutionResult::Halt { reason, gas_used } => {
            assert_eq!(gas_used, 123_456);

            match reason {
                GwynethHaltReason::GwynethHardFailure(hf) => {
                    assert_eq!(hf.chain_id, 1);
                    assert_eq!(hf.opcode, Some(revm::state::bytecode::opcode::CALL));
                    assert_eq!(
                        hf.code,
                        HardFailureCode::XcalloptionsMustBeFollowedByCallToExtensionOracle
                    );
                    assert_eq!(
                        hf.reason,
                        HardFailureCode::XcalloptionsMustBeFollowedByCallToExtensionOracle.reason()
                    );
                    assert_eq!(hf.gas_used, 123_456);
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
    let caller = Address::from([0x10; 20]);
    let entry = Address::from([0x13; 20]);
    let bad_to = Address::from([0x99; 20]);
    let target = Address::from([0x22; 20]);

    let mut l1 = InMemoryDB::default();
    let l2 = InMemoryDB::default();

    insert_eoa(&mut l1, caller);
    insert_code(
        &mut l1,
        entry,
        code_structural_violation(xcalloptions_word(2, target, false), bad_to),
    );

    let mut db = gwyneth_engine::build_l2_overlay_db_adapter(1, l1, std::iter::empty(), 1).expect("overlay db init must succeed");
    db.l2_overlays.insert(2, l2);

    let (block_env, cfg_env) = base_env();

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
    assert_eq!(out.result.gas_used(), 30_000_000);
    assert!(out.result.logs().is_empty());
    assert!(out.result.output().is_none());

    match out.result {
        ExecutionResult::Halt { reason, gas_used } => {
            assert_eq!(gas_used, 30_000_000);

            match reason {
                GwynethHaltReason::GwynethHardFailure(hf) => {
                    assert_eq!(hf.chain_id, 1);
                    assert_eq!(hf.opcode, Some(revm::state::bytecode::opcode::CALL));
                    assert_eq!(
                        hf.code,
                        HardFailureCode::XcalloptionsMustBeFollowedByCallToExtensionOracle
                    );
                    assert_eq!(
                        hf.reason,
                        HardFailureCode::XcalloptionsMustBeFollowedByCallToExtensionOracle.reason()
                    );
                    assert_eq!(hf.gas_used, 30_000_000);
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
    let caller = Address::from([0x10; 20]);
    let entry = Address::from([0x13; 20]);
    let bad_to = Address::from([0x99; 20]);
    let target = Address::from([0x22; 20]);

    let mut l1 = InMemoryDB::default();
    let l2 = InMemoryDB::default();

    insert_eoa(&mut l1, caller);
    insert_code(
        &mut l1,
        entry,
        code_structural_violation(xcalloptions_word(2, target, false), bad_to),
    );

    let mut db = gwyneth_engine::build_l2_overlay_db_adapter(1, l1, std::iter::empty(), 1).expect("overlay db init must succeed");
    db.l2_overlays.insert(2, l2);

    let (block_env, cfg_env) = base_env();

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory
        .for_surface(ExecutionSurface::TxSubmission)
        .create_evm(db, alloy_evm::EvmEnv { block_env, cfg_env });

    // Disabling the user inspector must not disable the always-on `JournalInspector` that enforces
    // structural invariants and reports hard-failure details.
    evm.set_inspector_enabled(false);

    let tx = TxEnv::builder()
        .chain_id(Some(1))
        .caller(caller)
        .kind(TxKind::Call(entry))
        .gas_limit(123_456)
        .gas_price(0)
        .value(U256::ZERO)
        .data(Bytes::new())
        .build()
        .expect("tx build");

    let out = evm.transact_raw(tx).expect("hard failure is normalized");
    assert!(!out.result.is_success());
}
