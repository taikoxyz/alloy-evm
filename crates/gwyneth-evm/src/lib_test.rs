use super::{compute_superrevert_fee_amounts, FeeSurfaceMathError, GwynethRunner};
use core::convert::Infallible;
use gwyneth_types::{ChainSwitchable, ParentLoadCheckpoints};
use revm::{
    context::{block::BlockEnv, cfg::CfgEnv},
    context_interface::result::{EVMError, InvalidTransaction},
    inspector::NoOpInspector,
    primitives::{Address, B256, U256},
    state::AccountInfo,
    Database,
};

#[derive(Debug)]
struct CountingDb {
    current_chain_id: u64,
    known_chain_ids: std::collections::BTreeSet<u64>,
    switch_attempts: Vec<u64>,
}

impl CountingDb {
    fn new(current_chain_id: u64, known_chain_ids: impl IntoIterator<Item = u64>) -> Self {
        Self {
            current_chain_id,
            known_chain_ids: known_chain_ids.into_iter().collect(),
            switch_attempts: Vec::new(),
        }
    }
}

impl ChainSwitchable for CountingDb {
    fn switch_to_chain(&mut self, chain_id: u64) -> Result<(), String> {
        self.switch_attempts.push(chain_id);
        if !self.known_chain_ids.contains(&chain_id) {
            return Err(format!("unknown chain id {chain_id}"));
        }
        self.current_chain_id = chain_id;
        Ok(())
    }

    fn current_chain_id(&self) -> u64 {
        self.current_chain_id
    }
}

impl ParentLoadCheckpoints for CountingDb {
    fn checkpoint(&self) -> usize {
        0
    }

    fn take_loads_since(&mut self, _checkpoint: usize) -> Vec<gwyneth_types::ParentLoadEntry> {
        Vec::new()
    }
}

impl Database for CountingDb {
    type Error = Infallible;

    fn basic(&mut self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(None)
    }

    fn code_by_hash(&mut self, _code_hash: B256) -> Result<revm::state::Bytecode, Self::Error> {
        Ok(revm::state::Bytecode::default())
    }

    fn storage(&mut self, _address: Address, _index: U256) -> Result<U256, Self::Error> {
        Ok(U256::ZERO)
    }

    fn block_hash(&mut self, _number: u64) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

#[test]
fn phase64_4b_superrevert_overflow_is_typed_fail_closed() {
    let err = compute_superrevert_fee_amounts(u128::MAX, 0, 2, true).expect_err("overflow must fail closed");
    assert_eq!(err, FeeSurfaceMathError::SuperrevertFeeOverflow);
}

#[test]
fn phase64_4b_superrevert_coinbase_underflow_is_typed_fail_closed() {
    let err = compute_superrevert_fee_amounts(10, 11, 1, true).expect_err("coinbase underflow must fail closed");
    assert_eq!(err, FeeSurfaceMathError::CoinbaseGasPriceUnderflow);
}

#[test]
fn phase64_4d_reset_for_new_tx_strict_failure_is_early_and_single_alignment_attempt() {
    let mut cfg_env = CfgEnv::default();
    cfg_env.chain_id = 1;
    let mut runner = GwynethRunner::from_env(
        CountingDb::new(1, [1]),
        alloy_evm::EvmEnv { block_env: BlockEnv::default(), cfg_env },
        NoOpInspector,
        gwyneth_detector::DetectorConfig::default(),
        gwyneth_types::ExecutionSurface::TxSubmission,
        true,
    );

    let err = runner
        .reset_for_new_tx(2, true)
        .expect_err("unknown strict origin chain must fail closed");
    assert!(
        matches!(err, EVMError::Transaction(InvalidTransaction::InvalidChainId)),
        "strict mode must map origin-chain alignment failure to InvalidChainId"
    );
    assert_eq!(
        runner.db().current_chain_id(),
        1,
        "failed early alignment must preserve the original chain context"
    );
    assert_eq!(
        runner.db().switch_attempts,
        vec![2],
        "origin alignment should be attempted exactly once on this failure path"
    );
}

