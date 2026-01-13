//! Precomputed block outcome transport types.
//!
//! These types allow a caller to supply a fully computed block outcome (receipts + state diff)
//! along with the deterministic commitment inputs needed to validate it at the host boundary.

use crate::block::BlockExecutionResult;
use alloy_primitives::{Bloom, B256};
use revm::database::states::BundleState;

/// Commitment inputs recomputed from block-local data (txs, receipts, etc).
///
/// This is a pure data carrier: comparisons are performed at a single choke point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonInputs {
    /// Root of the block's transaction list.
    pub tx_root: B256,
    /// Root of the block's receipt list (empty receipts are `root([])`).
    pub receipts_root: B256,
    /// Logs bloom derived from the receipt logs (empty receipts have the zero bloom).
    pub logs_bloom: Bloom,
    /// Total gas used by the block.
    pub gas_used: u64,
    /// Fork-conditional withdrawals root.
    pub withdrawals_root: Option<B256>,
    /// Fork-conditional blob gas used.
    pub blob_gas_used: Option<u64>,
    /// Fork-conditional requests hash.
    pub requests_hash: Option<B256>,
}

/// Minimal parent header view required for fork-derived field validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentHeaderView {
    /// Parent timestamp.
    pub timestamp: u64,
    /// Parent blob gas used.
    pub blob_gas_used: Option<u64>,
    /// Parent excess blob gas.
    pub excess_blob_gas: Option<u64>,
}

/// A validated, diff-backed block outcome that can be installed without executing transactions.
#[derive(Debug, Clone)]
pub struct PrecomputedBlockOutcome<Receipt> {
    /// The block execution result.
    pub result: BlockExecutionResult<Receipt>,
    /// The state diff/bundle to apply before computing the state root.
    pub bundle: BundleState,
    /// The commitment inputs recomputed from block-local data.
    pub comparison_inputs: ComparisonInputs,
    /// Minimal view of the parent header required for fork-derived field validation.
    pub parent_header: ParentHeaderView,
}
