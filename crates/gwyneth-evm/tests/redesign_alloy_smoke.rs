//! Redesign smoke test for `alloy-gwyneth-evm` wiring.

use alloy_evm::evm::BoundedEvmFactory as _;
use alloy_evm::Evm as _;
use alloy_gwyneth_evm::GwynethEvmFactoryImpl;
use gwyneth_types::ExecutionSurface;
use revm::{
    database::InMemoryDB,
    primitives::{Address, Bytes, U256},
};

#[test]
fn redesign_alloy_smoke() {
    let caller = Address::from([0x11; 20]);
    let recipient = Address::from([0x22; 20]);

    let mut l1 = InMemoryDB::default();
    gwyneth_phase64_shared_dev::gwyneth_test_insert_eoa!(l1, caller, 10_000_000_000u64);
    let l2 = InMemoryDB::default();

    let db = gwyneth_phase64_shared_dev::gwyneth_test_overlay_db_single_l2!(1u64, l1, 2u64, l2);
    let (block_env, cfg_env) =
        gwyneth_phase64_shared_dev::gwyneth_test_base_env_cancun!(1u64, Address::ZERO, 30_000_000);

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory
        .for_surface(ExecutionSurface::TxSubmission)
        .create_evm(db, alloy_evm::EvmEnv { block_env, cfg_env });

    let tx = gwyneth_phase64_shared_dev::gwyneth_test_tx_env_call!(
        1u64,
        caller,
        recipient,
        250_000,
        0,
        Some(0),
        U256::from(1),
        Bytes::new(),
        0
    );

    let out = evm.transact_raw(tx).expect("alloy-gwyneth-evm tx executes");
    assert!(out.result.is_success());
    assert!(out.result.logs().is_empty());
}
