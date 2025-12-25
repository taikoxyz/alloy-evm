use alloy_evm::Evm as _;
use alloy_gwyneth_evm::{GwynethEvmFactory, GwynethEvmFactoryImpl, GwynethHaltReason};
use gwyneth_engine::L2OverlayDb;
use gwyneth_types::EXTENSION_ORACLE;
use revm::{
    context::{block::BlockEnv, cfg::CfgEnv, tx::TxEnv},
    database::InMemoryDB,
    context_interface::result::ExecutionResult,
    primitives::{Address, Bytes, ChainAddress, HashMap, MultiChainTxKind, U256},
    state::{AccountInfo, Bytecode},
};

fn push_bytes(bytes: &[u8]) -> Vec<u8> {
    assert!(!bytes.is_empty());
    assert!(bytes.len() <= 32);
    let opcode = 0x5f_u8 + (bytes.len() as u8);
    let mut out = Vec::with_capacity(1 + bytes.len());
    out.push(opcode);
    out.extend_from_slice(bytes);
    out
}

fn push_u8(value: u8) -> Vec<u8> {
    push_bytes(&[value])
}

fn push_u16(value: u16) -> Vec<u8> {
    push_bytes(&value.to_be_bytes())
}

fn push_address(addr: Address) -> Vec<u8> {
    push_bytes(&addr.0[..])
}

fn xcalloptions_word(target_chain_id: u64, target: Address, direct: bool) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0..2].copy_from_slice(&1u16.to_be_bytes());
    out[2..10].copy_from_slice(&target_chain_id.to_be_bytes());
    out[10..30].copy_from_slice(&target.0[..]);
    out[30] = if direct { 1 } else { 0 };
    out
}

fn code_structural_violation(xcall_word: [u8; 32], bad_to: Address) -> Vec<u8> {
    let mut code = Vec::new();

    // mstore(0, xcall_word)
    code.extend(push_bytes(&xcall_word));
    code.extend(push_u8(0x00));
    code.push(0x52);

    // CALL XCALLOPTIONS (sets intent)
    code.extend(push_u8(0x04));
    code.extend(push_u8(0x00));
    code.extend(push_u8(0x1F));
    code.extend(push_u8(0x00));
    code.extend(push_u8(0x00));
    code.extend(push_u16(0x04D2));
    code.push(0x5A);
    code.push(0xF1);
    code.push(0x50);

    // Next CALL is not to EXTENSION_ORACLE -> hard failure.
    code.extend(push_u8(0x00)); // out_size
    code.extend(push_u8(0x00)); // out_offset
    code.extend(push_u8(0x00)); // in_size
    code.extend(push_u8(0x00)); // in_offset
    code.extend(push_u8(0x00)); // value
    code.extend(push_address(bad_to));
    code.push(0x5A);
    code.push(0xF1);

    // (unreachable)
    code.push(0x00);
    code
}

fn insert_code(db: &mut InMemoryDB, addr: Address, code: Vec<u8>) {
    let info = AccountInfo::default().with_code(Bytecode::new_raw(Bytes::from(code)));
    db.insert_account_info(addr, info);
}

fn insert_eoa(db: &mut InMemoryDB, addr: Address) {
    let info = AccountInfo::default().with_balance(U256::from(1_000_000u64));
    db.insert_account_info(addr, info);
}

fn base_env() -> (HashMap<u64, BlockEnv>, CfgEnv) {
    let mut cfg_env = CfgEnv::default();
    cfg_env.spec = revm::primitives::hardfork::SpecId::CANCUN;
    cfg_env.chain_id = 1;
    cfg_env.xchain = true;
    cfg_env.allow_mocking = true;
    cfg_env.parent_chain_id = Some(1);
    cfg_env.extension_oracle = Some(EXTENSION_ORACLE);
    cfg_env.gwyneth = Some(Address::from([0x33; 20]));

    let mut block_env = BlockEnv::default();
    block_env.beneficiary = ChainAddress::new(1, Address::ZERO);
    block_env.gas_limit = 30_000_000;

    let mut blocks: HashMap<u64, BlockEnv> = HashMap::default();
    blocks.insert(1, block_env.clone());
    blocks.insert(0, block_env);

    (blocks, cfg_env)
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

    let mut db = L2OverlayDb::new(l1);
    db.add_l2_overlay(2, l2);

    let (blocks, cfg_env) = base_env();

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory.create_gwyneth_evm(db, alloy_evm::EvmEnv { block_env: blocks, cfg_env });

    let tx = TxEnv::builder()
        .chain_id(Some(1))
        .caller(ChainAddress::new(1, caller))
        .kind(MultiChainTxKind::Call(ChainAddress::new(1, entry)))
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
        ExecutionResult::Halt { reason, gas_used, gas_used_per_chain, .. } => {
            assert_eq!(gas_used, 123_456);
            assert_eq!(gas_used_per_chain.get(&1).copied(), Some(123_456));

            match reason {
                GwynethHaltReason::GwynethHardFailure(hf) => {
                    assert_eq!(hf.chain_id, 1);
                    assert_eq!(hf.opcode, Some(revm::state::bytecode::opcode::CALL));
                    assert_eq!(
                        hf.reason,
                        "xcalloptions must be followed by CALL to extension oracle"
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

    let mut db = L2OverlayDb::new(l1);
    db.add_l2_overlay(2, l2);

    let (blocks, cfg_env) = base_env();

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory.create_gwyneth_evm(db, alloy_evm::EvmEnv { block_env: blocks, cfg_env });

    // Exercise the inspector-enabled path (the gwyneth journal inspector stays active either way).
    evm.set_inspector_enabled(true);

    let out = evm
        .transact_system_call(
            ChainAddress::new(1, caller),
            ChainAddress::new(1, entry),
            Bytes::new(),
        )
        .expect("hard failure is normalized");

    assert!(!out.result.is_success());
    assert_eq!(out.result.gas_used(), 30_000_000);
    assert!(out.result.logs().is_empty());
    assert!(out.result.output().is_none());

    match out.result {
        ExecutionResult::Halt { reason, gas_used, gas_used_per_chain, .. } => {
            assert_eq!(gas_used, 30_000_000);
            assert_eq!(gas_used_per_chain.get(&1).copied(), Some(30_000_000));

            match reason {
                GwynethHaltReason::GwynethHardFailure(hf) => {
                    assert_eq!(hf.chain_id, 1);
                    assert_eq!(hf.opcode, Some(revm::state::bytecode::opcode::CALL));
                    assert_eq!(
                        hf.reason,
                        "xcalloptions must be followed by CALL to extension oracle"
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
