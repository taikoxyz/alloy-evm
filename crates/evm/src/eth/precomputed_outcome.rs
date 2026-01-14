//! Precomputed block outcome transport types.
//!
//! These types allow a caller to supply a fully computed block outcome (receipts + state diff)
//! along with the deterministic commitment inputs needed to validate it at the host boundary.
//! Fork-derived header field validation requires a minimal view of the parent header, supplied at
//! the host boundary (not embedded in the precomputed outcome).

use crate::block::BlockExecutionResult;
use alloy_consensus::BlockHeader;
use alloy_eips::eip7840::BlobParams;
use alloy_hardforks::{EthereumHardfork, EthereumHardforks};
use alloy_primitives::{Bloom, B256};
use core::fmt::Debug;
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
    /// Parent base fee per gas, with missing values treated as 0.
    pub base_fee_per_gas: u64,
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
}

/// Minimal header view used by [`canonicalize_and_compare`].
pub trait HeaderView {
    /// State root committed in the header.
    fn state_root(&self) -> B256;
    /// Transactions root committed in the header.
    fn transactions_root(&self) -> B256;
    /// Receipts root committed in the header.
    fn receipts_root(&self) -> B256;
    /// Logs bloom committed in the header.
    fn logs_bloom(&self) -> Bloom;
    /// Gas used committed in the header.
    fn gas_used(&self) -> u64;
    /// Fork-conditional withdrawals root.
    fn withdrawals_root(&self) -> Option<B256>;
    /// Fork-conditional blob gas used.
    fn blob_gas_used(&self) -> Option<u64>;
    /// Fork-conditional excess blob gas.
    fn excess_blob_gas(&self) -> Option<u64>;
    /// Fork-conditional requests hash.
    fn requests_hash(&self) -> Option<B256>;
    /// Extra-data bytes committed by the header.
    fn extra_data(&self) -> &[u8];
}

impl<T> HeaderView for T
where
    T: BlockHeader + ?Sized,
{
    fn state_root(&self) -> B256 {
        BlockHeader::state_root(self)
    }

    fn transactions_root(&self) -> B256 {
        BlockHeader::transactions_root(self)
    }

    fn receipts_root(&self) -> B256 {
        BlockHeader::receipts_root(self)
    }

    fn logs_bloom(&self) -> Bloom {
        BlockHeader::logs_bloom(self)
    }

    fn gas_used(&self) -> u64 {
        BlockHeader::gas_used(self)
    }

    fn withdrawals_root(&self) -> Option<B256> {
        BlockHeader::withdrawals_root(self)
    }

    fn blob_gas_used(&self) -> Option<u64> {
        BlockHeader::blob_gas_used(self)
    }

    fn excess_blob_gas(&self) -> Option<u64> {
        BlockHeader::excess_blob_gas(self)
    }

    fn requests_hash(&self) -> Option<B256> {
        BlockHeader::requests_hash(self)
    }

    fn extra_data(&self) -> &[u8] {
        BlockHeader::extra_data(self).as_ref()
    }
}

/// Fork schedule interface required by [`canonicalize_and_compare`].
///
/// This deliberately avoids reth-specific chain spec types by using `alloy-hardforks` primitives.
pub trait ForkSchedule: Debug {
    /// Returns `true` if the given fork is active at `(block_number, timestamp)`.
    fn is_fork_active_at(&self, fork: EthereumHardfork, block_number: u64, timestamp: u64) -> bool;

    /// Returns blob parameters to use for `(block_number, timestamp)` when Cancun is active.
    fn blob_params_at(&self, block_number: u64, timestamp: u64) -> BlobParams;
}

impl<T> ForkSchedule for T
where
    T: EthereumHardforks + Debug,
{
    fn is_fork_active_at(&self, fork: EthereumHardfork, block_number: u64, timestamp: u64) -> bool {
        self.ethereum_fork_activation(fork).active_at_timestamp_or_number(timestamp, block_number)
    }

    fn blob_params_at(&self, block_number: u64, timestamp: u64) -> BlobParams {
        // Prefer the most recent blob-params fork that is active. Missing forks default to
        // `ForkCondition::Never` via `EthereumHardforks`.
        if self.is_fork_active_at(EthereumHardfork::Bpo5, block_number, timestamp) {
            return BlobParams::bpo2();
        }
        if self.is_fork_active_at(EthereumHardfork::Bpo4, block_number, timestamp) {
            return BlobParams::bpo2();
        }
        if self.is_fork_active_at(EthereumHardfork::Bpo3, block_number, timestamp) {
            return BlobParams::bpo2();
        }
        if self.is_fork_active_at(EthereumHardfork::Bpo2, block_number, timestamp) {
            return BlobParams::bpo2();
        }
        if self.is_fork_active_at(EthereumHardfork::Bpo1, block_number, timestamp) {
            return BlobParams::bpo1();
        }
        if self.is_fork_active_at(EthereumHardfork::Osaka, block_number, timestamp) {
            return BlobParams::osaka();
        }
        if self.is_fork_active_at(EthereumHardfork::Prague, block_number, timestamp) {
            return BlobParams::prague();
        }
        BlobParams::cancun()
    }
}

/// Validation failures for the host-boundary precomputed-outcome contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrecomputedOutcomeValidationError {
    /// The `state_root` does not match the recomputed value.
    #[error("state_root mismatch: expected={expected} got={got}")]
    StateRootMismatch {
        /// Expected `state_root`.
        expected: B256,
        /// Observed `state_root`.
        got: B256,
    },
    /// The `transactions_root` does not match the recomputed value.
    #[error("transactions_root mismatch: expected={expected} got={got}")]
    TransactionsRootMismatch {
        /// Expected `transactions_root`.
        expected: B256,
        /// Observed `transactions_root`.
        got: B256,
    },
    /// The `receipts_root` does not match the recomputed value.
    #[error("receipts_root mismatch: expected={expected} got={got}")]
    ReceiptsRootMismatch {
        /// Expected `receipts_root`.
        expected: B256,
        /// Observed `receipts_root`.
        got: B256,
    },
    /// The `logs_bloom` does not match the recomputed value.
    #[error("logs_bloom mismatch")]
    LogsBloomMismatch {
        /// Expected logs bloom.
        expected: Bloom,
        /// Observed logs bloom.
        got: Bloom,
    },
    /// The `gas_used` does not match the recomputed value.
    #[error("gas_used mismatch: expected={expected} got={got}")]
    GasUsedMismatch {
        /// Expected gas used.
        expected: u64,
        /// Observed gas used.
        got: u64,
    },

    /// The `extra_data` does not match the expected bytes.
    #[error("extra_data mismatch: expected_len={expected_len} got_len={got_len}")]
    ExtraDataMismatch {
        /// Expected extra-data length.
        expected_len: usize,
        /// Observed extra-data length.
        got_len: usize,
    },

    /// The `withdrawals_root` is required but missing.
    #[error("withdrawals_root missing")]
    WithdrawalsRootMissing,
    /// The `withdrawals_root` was set when withdrawals are inactive.
    #[error("unexpected withdrawals_root")]
    WithdrawalsRootUnexpected,
    /// The `withdrawals_root` does not match the recomputed value.
    #[error("withdrawals_root mismatch: expected={expected} got={got}")]
    WithdrawalsRootMismatch {
        /// Expected withdrawals root.
        expected: B256,
        /// Observed withdrawals root.
        got: B256,
    },

    /// The `blob_gas_used` is required but missing.
    #[error("blob_gas_used missing")]
    BlobGasUsedMissing,
    /// The `blob_gas_used` was set when blob gas is inactive.
    #[error("unexpected blob_gas_used")]
    BlobGasUsedUnexpected,
    /// The `blob_gas_used` does not match the recomputed value.
    #[error("blob_gas_used mismatch: expected={expected} got={got}")]
    BlobGasUsedMismatch {
        /// Expected blob gas used.
        expected: u64,
        /// Observed blob gas used.
        got: u64,
    },

    /// The `excess_blob_gas` is required but missing.
    #[error("excess_blob_gas missing")]
    ExcessBlobGasMissing,
    /// The `excess_blob_gas` was set when blob gas is inactive.
    #[error("unexpected excess_blob_gas")]
    ExcessBlobGasUnexpected,
    /// The `excess_blob_gas` does not match the recomputed value.
    #[error(
        "excess_blob_gas mismatch: expected={expected} got={got} (parent_excess_blob_gas={parent_excess_blob_gas}, parent_blob_gas_used={parent_blob_gas_used})"
    )]
    ExcessBlobGasMismatch {
        /// Expected excess blob gas.
        expected: u64,
        /// Observed excess blob gas.
        got: u64,
        /// Parent header excess blob gas used for derivation.
        parent_excess_blob_gas: u64,
        /// Parent header blob gas used for derivation.
        parent_blob_gas_used: u64,
    },

    /// The `requests_hash` is required but missing.
    #[error("requests_hash missing")]
    RequestsHashMissing,
    /// The `requests_hash` was set when requests are inactive.
    #[error("unexpected requests_hash")]
    RequestsHashUnexpected,
    /// The `requests_hash` does not match the recomputed value.
    #[error("requests_hash mismatch: expected={expected} got={got}")]
    RequestsHashMismatch {
        /// Expected requests hash.
        expected: B256,
        /// Observed requests hash.
        got: B256,
    },
}

/// Canonicalize and compare the DA-provided header commitments against recomputed inputs.
///
/// This is the single choke point for fork-optional field normalization and commitment equality
/// checks for precomputed outcomes.
#[expect(clippy::too_many_arguments)]
pub fn canonicalize_and_compare<H, S>(
    da_header: &H,
    inputs: &ComparisonInputs,
    state_root: B256,
    block_number: u64,
    timestamp: u64,
    expected_extra_data: &[u8],
    parent_header: ParentHeaderView,
    fork_schedule: &S,
) -> Result<(), PrecomputedOutcomeValidationError>
where
    H: HeaderView + ?Sized,
    S: ForkSchedule + ?Sized,
{
    if da_header.state_root() != state_root {
        return Err(PrecomputedOutcomeValidationError::StateRootMismatch {
            expected: state_root,
            got: da_header.state_root(),
        });
    }

    let da_extra_data = HeaderView::extra_data(da_header);
    if da_extra_data != expected_extra_data {
        return Err(PrecomputedOutcomeValidationError::ExtraDataMismatch {
            expected_len: expected_extra_data.len(),
            got_len: da_extra_data.len(),
        });
    }

    if da_header.transactions_root() != inputs.tx_root {
        return Err(PrecomputedOutcomeValidationError::TransactionsRootMismatch {
            expected: inputs.tx_root,
            got: da_header.transactions_root(),
        });
    }

    if da_header.receipts_root() != inputs.receipts_root {
        return Err(PrecomputedOutcomeValidationError::ReceiptsRootMismatch {
            expected: inputs.receipts_root,
            got: da_header.receipts_root(),
        });
    }

    if da_header.logs_bloom() != inputs.logs_bloom {
        return Err(PrecomputedOutcomeValidationError::LogsBloomMismatch {
            expected: inputs.logs_bloom,
            got: da_header.logs_bloom(),
        });
    }

    if da_header.gas_used() != inputs.gas_used {
        return Err(PrecomputedOutcomeValidationError::GasUsedMismatch {
            expected: inputs.gas_used,
            got: da_header.gas_used(),
        });
    }

    let is_shanghai =
        fork_schedule.is_fork_active_at(EthereumHardfork::Shanghai, block_number, timestamp);
    match (is_shanghai, da_header.withdrawals_root(), inputs.withdrawals_root) {
        (false, None, None) => {}
        (false, Some(_), _) | (false, _, Some(_)) => {
            return Err(PrecomputedOutcomeValidationError::WithdrawalsRootUnexpected);
        }
        (true, None, _) | (true, _, None) => {
            return Err(PrecomputedOutcomeValidationError::WithdrawalsRootMissing);
        }
        (true, Some(got), Some(expected)) if got == expected => {}
        (true, Some(got), Some(expected)) => {
            return Err(PrecomputedOutcomeValidationError::WithdrawalsRootMismatch { expected, got });
        }
    }

    let is_cancun = fork_schedule.is_fork_active_at(EthereumHardfork::Cancun, block_number, timestamp);
    match (is_cancun, da_header.blob_gas_used(), inputs.blob_gas_used) {
        (false, None, None) => {}
        (false, Some(_), _) | (false, _, Some(_)) => {
            return Err(PrecomputedOutcomeValidationError::BlobGasUsedUnexpected);
        }
        (true, None, _) | (true, _, None) => {
            return Err(PrecomputedOutcomeValidationError::BlobGasUsedMissing);
        }
        (true, Some(got), Some(expected)) if got == expected => {}
        (true, Some(got), Some(expected)) => {
            return Err(PrecomputedOutcomeValidationError::BlobGasUsedMismatch { expected, got });
        }
    }

    match (is_cancun, da_header.excess_blob_gas()) {
        (false, None) => {}
        (false, Some(_)) => {
            return Err(PrecomputedOutcomeValidationError::ExcessBlobGasUnexpected);
        }
        (true, None) => {
            return Err(PrecomputedOutcomeValidationError::ExcessBlobGasMissing);
        }
        (true, Some(got)) => {
            let parent_number = block_number.saturating_sub(1);
            let parent_timestamp = parent_header.timestamp;
            let is_parent_cancun = fork_schedule.is_fork_active_at(
                EthereumHardfork::Cancun,
                parent_number,
                parent_timestamp,
            );

            let (parent_excess_blob_gas, parent_blob_gas_used) = if is_parent_cancun {
                (
                    parent_header.excess_blob_gas.unwrap_or_default(),
                    parent_header.blob_gas_used.unwrap_or_default(),
                )
            } else {
                (0, 0)
            };

            let params = fork_schedule.blob_params_at(block_number, timestamp);
            let expected = params.next_block_excess_blob_gas_osaka(
                parent_excess_blob_gas,
                parent_blob_gas_used,
                parent_header.base_fee_per_gas,
            );

            if got != expected {
                return Err(PrecomputedOutcomeValidationError::ExcessBlobGasMismatch {
                    expected,
                    got,
                    parent_excess_blob_gas,
                    parent_blob_gas_used,
                });
            }
        }
    }

    let is_prague = fork_schedule.is_fork_active_at(EthereumHardfork::Prague, block_number, timestamp);
    match (is_prague, da_header.requests_hash(), inputs.requests_hash) {
        (false, None, None) => {}
        (false, Some(_), _) | (false, _, Some(_)) => {
            return Err(PrecomputedOutcomeValidationError::RequestsHashUnexpected);
        }
        (true, None, _) | (true, _, None) => {
            return Err(PrecomputedOutcomeValidationError::RequestsHashMissing);
        }
        (true, Some(got), Some(expected)) if got == expected => {}
        (true, Some(got), Some(expected)) => {
            return Err(PrecomputedOutcomeValidationError::RequestsHashMismatch { expected, got });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{Header, EMPTY_OMMER_ROOT_HASH};
    use alloy_primitives::{Address, Bytes, U256};

    fn test_header() -> Header {
        Header {
            parent_hash: B256::ZERO,
            ommers_hash: EMPTY_OMMER_ROOT_HASH,
            beneficiary: Address::ZERO,
            state_root: B256::ZERO,
            transactions_root: B256::ZERO,
            receipts_root: B256::ZERO,
            withdrawals_root: None,
            logs_bloom: Bloom::ZERO,
            timestamp: 0,
            mix_hash: B256::ZERO,
            nonce: 0u64.into(),
            base_fee_per_gas: Some(0),
            number: 0,
            gas_limit: 30_000_000,
            difficulty: U256::ZERO,
            gas_used: 0,
            extra_data: Bytes::new(),
            parent_beacon_block_root: None,
            blob_gas_used: None,
            excess_blob_gas: None,
            requests_hash: None,
        }
    }

    #[derive(Debug)]
    struct TestSchedule;

    impl EthereumHardforks for TestSchedule {
        fn ethereum_fork_activation(&self, fork: EthereumHardfork) -> alloy_hardforks::ForkCondition {
            use alloy_hardforks::ForkCondition;
            match fork {
                EthereumHardfork::Shanghai => ForkCondition::Timestamp(10),
                EthereumHardfork::Cancun => ForkCondition::Timestamp(20),
                EthereumHardfork::Prague => ForkCondition::Timestamp(30),
                _ => ForkCondition::Never,
            }
        }
    }

    #[test]
    fn canonicalize_rejects_withdrawals_root_when_shanghai_inactive() {
        let schedule = TestSchedule;
        let mut header = test_header();
        header.timestamp = 5;
        header.state_root = B256::from([0x11; 32]);
        header.withdrawals_root = Some(B256::from([0x22; 32]));

        let inputs = ComparisonInputs {
            tx_root: header.transactions_root,
            receipts_root: header.receipts_root,
            logs_bloom: header.logs_bloom,
            gas_used: header.gas_used,
            withdrawals_root: None,
            blob_gas_used: None,
            requests_hash: None,
        };

        let err = canonicalize_and_compare(
            &header,
            &inputs,
            header.state_root,
            header.number,
            header.timestamp,
            header.extra_data.as_ref(),
            ParentHeaderView { timestamp: 0, blob_gas_used: None, excess_blob_gas: None, base_fee_per_gas: 0 },
            &schedule,
        )
        .unwrap_err();

        assert_eq!(err, PrecomputedOutcomeValidationError::WithdrawalsRootUnexpected);
    }

    #[test]
    fn canonicalize_rejects_missing_blob_fields_when_cancun_active() {
        let schedule = TestSchedule;
        let mut header = test_header();
        header.timestamp = 25;
        header.number = 42;
        header.state_root = B256::from([0x11; 32]);
        header.withdrawals_root = Some(B256::from([0x22; 32]));
        header.blob_gas_used = None;
        header.excess_blob_gas = None;

        let inputs = ComparisonInputs {
            tx_root: header.transactions_root,
            receipts_root: header.receipts_root,
            logs_bloom: header.logs_bloom,
            gas_used: header.gas_used,
            withdrawals_root: header.withdrawals_root,
            blob_gas_used: Some(0),
            requests_hash: None,
        };

        let err = canonicalize_and_compare(
            &header,
            &inputs,
            header.state_root,
            header.number,
            header.timestamp,
            header.extra_data.as_ref(),
            ParentHeaderView { timestamp: 0, blob_gas_used: None, excess_blob_gas: None, base_fee_per_gas: 0 },
            &schedule,
        )
        .unwrap_err();

        assert_eq!(err, PrecomputedOutcomeValidationError::BlobGasUsedMissing);
    }

    #[test]
    fn canonicalize_accepts_first_post_cancun_excess_blob_gas_derivation() {
        let schedule = TestSchedule;
        let mut header = test_header();
        header.timestamp = 25;
        header.number = 42;
        header.state_root = B256::from([0x11; 32]);
        header.withdrawals_root = Some(B256::from([0x22; 32]));
        header.blob_gas_used = Some(0);
        header.excess_blob_gas = Some(0);

        let inputs = ComparisonInputs {
            tx_root: header.transactions_root,
            receipts_root: header.receipts_root,
            logs_bloom: header.logs_bloom,
            gas_used: header.gas_used,
            withdrawals_root: header.withdrawals_root,
            blob_gas_used: Some(0),
            requests_hash: None,
        };

        canonicalize_and_compare(
            &header,
            &inputs,
            header.state_root,
            header.number,
            header.timestamp,
            header.extra_data.as_ref(),
            ParentHeaderView { timestamp: 15, blob_gas_used: None, excess_blob_gas: None, base_fee_per_gas: 0 },
            &schedule,
        )
        .expect("first post-cancun excess_blob_gas uses zero parent values");
    }

    #[test]
    fn canonicalize_rejects_extra_data_mismatch() {
        let schedule = TestSchedule;
        let mut header = test_header();
        header.timestamp = 5;
        header.state_root = B256::from([0x11; 32]);
        header.extra_data = Bytes::from(vec![0x01, 0x02]);

        let inputs = ComparisonInputs {
            tx_root: header.transactions_root,
            receipts_root: header.receipts_root,
            logs_bloom: header.logs_bloom,
            gas_used: header.gas_used,
            withdrawals_root: None,
            blob_gas_used: None,
            requests_hash: None,
        };

        let err = canonicalize_and_compare(
            &header,
            &inputs,
            header.state_root,
            header.number,
            header.timestamp,
            &[0x03, 0x04],
            ParentHeaderView { timestamp: 0, blob_gas_used: None, excess_blob_gas: None, base_fee_per_gas: 0 },
            &schedule,
        )
        .unwrap_err();

        assert_eq!(err, PrecomputedOutcomeValidationError::ExtraDataMismatch { expected_len: 2, got_len: 2 });
    }
}
