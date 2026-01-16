//! Phase 39.3 drift guard: ensure `before_call(..)` is invoked at runtime and is forwarded through
//! inspector wrappers (`GwynethInspector` + `either::Either`).

use alloy_evm::evm::BoundedEvmFactory as _;
use alloy_evm::Evm as _;
use alloy_gwyneth_evm::GwynethEvmFactoryImpl;
use either::Either;
use gwyneth_engine::L2OverlayDb;
use gwyneth_types::ExecutionSurface;
use revm::{
    context::{block::BlockEnv, cfg::CfgEnv, tx::TxEnv},
    database::InMemoryDB,
    inspector::{Inspector, NoOpInspector},
    interpreter::CallInputs,
    primitives::{Address, Bytes, TxKind, U256},
    state::{bytecode::opcode, AccountInfo, Bytecode},
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug, Clone)]
struct BeforeCallSpy {
    callee: Address,
    seen: Arc<AtomicBool>,
}

impl<CTX> Inspector<CTX> for BeforeCallSpy {
    fn before_call(&mut self, _context: &mut CTX, inputs: &mut CallInputs) {
        if inputs.target_address == self.callee {
            self.seen.store(true, Ordering::SeqCst);
        }
    }
}

fn insert_code(db: &mut InMemoryDB, addr: Address, code: Vec<u8>) {
    let info = AccountInfo::default().with_code(Bytecode::new_raw(Bytes::from(code)));
    db.insert_account_info(addr, info);
}

fn insert_eoa(db: &mut InMemoryDB, addr: Address) {
    let info = AccountInfo::default().with_balance(U256::from(1_000_000_000_000u64));
    db.insert_account_info(addr, info);
}

fn code_internal_call(to: Address) -> Vec<u8> {
    let mut code = Vec::new();
    code.extend([opcode::PUSH1, 0x00]); // retSize
    code.extend([opcode::PUSH1, 0x00]); // retOffset
    code.extend([opcode::PUSH1, 0x00]); // argsSize
    code.extend([opcode::PUSH1, 0x00]); // argsOffset
    code.extend([opcode::PUSH1, 0x00]); // value
    code.push(opcode::PUSH20);
    code.extend_from_slice(&to.0[..]);
    code.extend([opcode::PUSH2, 0xFF, 0xFF]); // gas
    code.push(opcode::CALL);
    code.push(opcode::STOP);
    code
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

fn run_with_inspector<I>(
    inspector: I,
    before_call_seen: Arc<AtomicBool>,
    caller: Address,
    entry: Address,
    callee: Address,
) where
    I: Inspector<alloy_gwyneth_evm::GwynethEvmContext<L2OverlayDb<InMemoryDB, InMemoryDB>>>,
{
    let mut l1 = InMemoryDB::default();
    let l2 = InMemoryDB::default();

    insert_eoa(&mut l1, caller);
    insert_code(&mut l1, callee, vec![opcode::STOP]);
    insert_code(&mut l1, entry, code_internal_call(callee));

    let mut db = L2OverlayDb::new(1, l1);
    db.add_l2_overlay(2, l2);

    let (block_env, cfg_env) = base_env();
    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory
        .for_surface(ExecutionSurface::TxSubmission)
        .create_evm_with_inspector(db, alloy_evm::EvmEnv { block_env, cfg_env }, inspector);
    evm.set_inspector_enabled(true);

    let tx = TxEnv::builder()
        .chain_id(Some(1))
        .caller(caller)
        .kind(TxKind::Call(entry))
        .gas_limit(100_000)
        .gas_price(0)
        .value(U256::ZERO)
        .data(Bytes::new())
        .build()
        .expect("tx build");

    let out = evm.transact_raw(tx).expect("tx executes");
    assert!(out.result.is_success());
    assert!(
        before_call_seen.load(Ordering::SeqCst),
        "expected before_call to be forwarded to the user inspector"
    );
}

#[test]
fn phase39_3_before_call_forwarded_through_gwyneth_inspector_and_either() {
    let caller = Address::from([0x10; 20]);
    let entry = Address::from([0x13; 20]);
    let callee = Address::from([0x42; 20]);

    // Case A: `either::Either::Left` forwarding.
    let left_seen = Arc::new(AtomicBool::new(false));
    let left_inspector: Either<BeforeCallSpy, NoOpInspector> =
        Either::Left(BeforeCallSpy { callee, seen: left_seen.clone() });
    run_with_inspector(left_inspector, left_seen, caller, entry, callee);

    // Case B: `either::Either::Right` forwarding.
    let right_seen = Arc::new(AtomicBool::new(false));
    let right_inspector: Either<NoOpInspector, BeforeCallSpy> =
        Either::Right(BeforeCallSpy { callee, seen: right_seen.clone() });
    run_with_inspector(right_inspector, right_seen, caller, entry, callee);
}
