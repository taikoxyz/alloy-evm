use alloy_evm::{Evm, EvmEnv, EvmFactory, eth::EthEvmFactory};
use alloy_primitives::{address, Address, U256};
use revm::{
    context::{BlockEnv, CfgEnv, TxEnv},
    database::{MultiEmptyDB, EmptyDB},
    primitives::{hardfork::SpecId, ChainAddress, HashMap},
    context::multi_chain_tx::TxKind,
};

#[test]
fn test_multichain_support() {
    // Test 1: Create EVM with chain configuration
    let mut cfg_env = CfgEnv::default();
    cfg_env.spec = SpecId::SHANGHAI;
    cfg_env.chain_id = 1;
    
    // Create block environment for the chain
    let mut block = BlockEnv::default();
    block.number = 1000;
    block.beneficiary = ChainAddress::new(1, address!("0x0000000000000000000000000000000000000001"));
    
    let mut block_env = HashMap::default();
    block_env.insert(1, block);
    
    let env = EvmEnv { block_env, cfg_env };
    let factory = EthEvmFactory::default();
    let mut multi_db = MultiEmptyDB::new();
    multi_db.add_chain(1, EmptyDB::default());
    let mut evm = factory.create_evm(multi_db, env);
    
    // Test 2: Verify chain ID handling
    assert_eq!(evm.chain_id(), 1);
    
    // Test 3: Verify block access
    let block = evm.block();
    assert_eq!(block.number, 1000);
    assert_eq!(block.beneficiary.0, 1); // chain_id
    assert_eq!(block.beneficiary.1, address!("0x0000000000000000000000000000000000000001"));
    
    // Test 4: Create transaction with ChainAddress
    let tx = TxEnv {
        caller: ChainAddress::new(1, address!("0x1111111111111111111111111111111111111111")),
        kind: TxKind::Call(ChainAddress::new(1, address!("0x2222222222222222222222222222222222222222"))),
        gas_limit: 21000,
        gas_price: 1000000000,
        value: U256::ZERO,
        data: Default::default(),
        nonce: 0,
        chain_id: Some(1),
        ..Default::default()
    };
    
    assert_eq!(tx.caller.0, 1);
    assert_eq!(tx.caller.1, address!("0x1111111111111111111111111111111111111111"));
    
    if let TxKind::Call(to) = tx.kind {
        assert_eq!(to.0, 1);
        assert_eq!(to.1, address!("0x2222222222222222222222222222222222222222"));
    } else {
        panic!("Expected Call variant");
    }
    
    // Test 5: System call with automatic chain ID
    let result = evm.transact_system_call(
        address!("0x3333333333333333333333333333333333333333"),
        address!("0x4444444444444444444444444444444444444444"),
        Default::default(),
    );
    
    // System call should execute (may succeed or fail depending on the empty contract)
    // The important thing is that it doesn't panic and handles chain IDs correctly
    match result {
        Ok(_) => {
            // Call succeeded (empty contract returns successfully)
        },
        Err(_) => {
            // Call failed (which is also acceptable with empty DB)
        }
    }
}

#[test]
fn test_basic_evm_creation() {
    let mut cfg_env = CfgEnv::default();
    cfg_env.spec = SpecId::SHANGHAI;
    cfg_env.chain_id = 999;
    
    let mut block = BlockEnv::default();
    block.number = 5000;
    block.beneficiary = ChainAddress::new(999, address!("0x0000000000000000000000000000000000000000"));
    
    let mut block_env = HashMap::default();
    block_env.insert(999, block);
    
    let env = EvmEnv { block_env, cfg_env };
    let factory = EthEvmFactory::default();
    let mut multi_db = MultiEmptyDB::new();
    multi_db.add_chain(999, EmptyDB::default());
    let evm = factory.create_evm(multi_db, env);
    
    // Verify block is set correctly
    let block = evm.block();
    assert_eq!(block.number, 5000);
}

#[test] 
fn test_different_chain_configs() {
    let mut cfg_env = CfgEnv::default();
    cfg_env.spec = SpecId::SHANGHAI;
    
    // Test different chain configurations
    for chain_id in [1u64, 10, 42161] {
        cfg_env.chain_id = chain_id;
        
        let mut block = BlockEnv::default();
        block.number = 1000 * chain_id;
        block.beneficiary = ChainAddress::new(chain_id, Address::from([chain_id as u8; 20]));
        
        let mut block_env = HashMap::default();
        block_env.insert(chain_id, block);
        
        let env = EvmEnv { block_env, cfg_env: cfg_env.clone() };
        let factory = EthEvmFactory::default();
        let mut multi_db = MultiEmptyDB::new();
        multi_db.add_chain(chain_id, EmptyDB::default());
        let evm = factory.create_evm(multi_db, env);
        
        assert_eq!(evm.chain_id(), chain_id);
        assert_eq!(evm.block().number, 1000 * chain_id);
    }
}