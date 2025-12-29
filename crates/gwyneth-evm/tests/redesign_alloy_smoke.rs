//! Redesign smoke test for `alloy-gwyneth-evm` wiring.

use alloy_evm::Evm as _;
use alloy_gwyneth_evm::{GwynethEvmFactory, GwynethEvmFactoryImpl};
use gwyneth_engine::L2OverlayDb;
use revm::{
    context::{block::BlockEnv, cfg::CfgEnv, tx::TxEnv},
    database::InMemoryDB,
    primitives::{Address, Bytes, HashMap, TxKind, U256},
    state::AccountInfo,
};

#[test]
fn redesign_alloy_smoke() {
    let caller = Address::from([0x11; 20]);
    let recipient = Address::from([0x22; 20]);

    let mut l1 = InMemoryDB::default();
    l1.insert_account_info(
        caller,
        AccountInfo { balance: U256::from(10_000_000_000u64), ..Default::default() },
    );
    let l2 = InMemoryDB::default();

    let mut db = L2OverlayDb::new(1, l1);
    db.add_l2_overlay(2, l2);

    let mut cfg_env = CfgEnv::default();
    cfg_env.spec = revm::primitives::hardfork::SpecId::CANCUN;
    cfg_env.chain_id = 1;

    let mut block_env = BlockEnv::default();
    block_env.beneficiary = Address::ZERO;
    block_env.gas_limit = 30_000_000;

    let mut blocks: HashMap<u64, BlockEnv> = HashMap::default();
    blocks.insert(1, block_env.clone());
    blocks.insert(0, block_env);

    let factory = GwynethEvmFactoryImpl::default();
    let mut evm = factory.create_gwyneth_evm(db, alloy_evm::EvmEnv { block_env: blocks, cfg_env });

    let mut tx = TxEnv::default();
    tx.caller = caller;
    tx.kind = TxKind::Call(recipient);
    tx.chain_id = Some(1);
    tx.gas_limit = 250_000;
    tx.gas_price = 0;
    tx.gas_priority_fee = Some(0);
    tx.value = U256::from(1);
    tx.data = Bytes::new();
    tx.nonce = 0;

    let out = evm.transact_raw(tx).expect("alloy-gwyneth-evm tx executes");
    assert!(out.result.is_success());
    assert!(out.result.logs().is_empty());
}
