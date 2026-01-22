//! Compile-time drift checks for upstream-ish `alloy-evm` shapes.
//!
//! Phase 13.6a contract: shared deps must stay single-chain (no multichain env maps or
//! gwyneth-only fields in vanilla types).

use alloy_evm::{block::BlockExecutionResult, EvmEnv};

#[test]
fn alloy_evm_env_is_single_chain() {
    let env: EvmEnv = Default::default();

    // No `..` on purpose: fails to compile if new fields appear.
    let EvmEnv { cfg_env: _, block_env: _ } = env;
}

#[test]
fn alloy_block_execution_result_is_vanilla() {
    let result: BlockExecutionResult<()> = Default::default();

    // No `..` on purpose: fails to compile if new fields appear.
    let BlockExecutionResult { receipts: _, requests: _, gas_used: _, blob_gas_used: _ } = result;
}
