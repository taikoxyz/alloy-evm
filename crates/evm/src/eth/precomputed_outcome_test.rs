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

#[test]
fn canonicalize_rejects_state_root_mismatch() {
    let schedule = TestSchedule;
    let mut header = test_header();
    header.timestamp = 5;
    header.state_root = B256::from([0x11; 32]);

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
        B256::from([0x22; 32]),
        header.number,
        header.timestamp,
        header.extra_data.as_ref(),
        ParentHeaderView { timestamp: 0, blob_gas_used: None, excess_blob_gas: None, base_fee_per_gas: 0 },
        &schedule,
    )
    .unwrap_err();

    assert!(matches!(err, PrecomputedOutcomeValidationError::StateRootMismatch { .. }));
}

#[test]
fn canonicalize_rejects_receipts_root_mismatch_for_empty_receipts_form() {
    let schedule = TestSchedule;
    let mut header = test_header();
    header.timestamp = 5;
    header.state_root = B256::from([0x11; 32]);
    header.receipts_root = B256::from([0x22; 32]);

    let inputs = ComparisonInputs {
        tx_root: header.transactions_root,
        receipts_root: B256::from([0x33; 32]),
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

    assert!(matches!(err, PrecomputedOutcomeValidationError::ReceiptsRootMismatch { .. }));
}

#[test]
fn canonicalize_rejects_blob_gas_used_when_cancun_inactive() {
    let schedule = TestSchedule;
    let mut header = test_header();
    header.timestamp = 5;
    header.state_root = B256::from([0x11; 32]);
    header.blob_gas_used = Some(0);

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

    assert_eq!(err, PrecomputedOutcomeValidationError::BlobGasUsedUnexpected);
}

#[test]
fn canonicalize_rejects_excess_blob_gas_when_cancun_inactive() {
    let schedule = TestSchedule;
    let mut header = test_header();
    header.timestamp = 5;
    header.number = 42;
    header.state_root = B256::from([0x11; 32]);
    header.excess_blob_gas = Some(0);

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

    assert_eq!(err, PrecomputedOutcomeValidationError::ExcessBlobGasUnexpected);
}

#[test]
fn canonicalize_rejects_requests_hash_when_prague_inactive() {
    let schedule = TestSchedule;
    let mut header = test_header();
    header.timestamp = 0;
    header.state_root = B256::from([0x11; 32]);
    header.requests_hash = Some(B256::from([0x22; 32]));

    let inputs = ComparisonInputs {
        tx_root: header.transactions_root,
        receipts_root: header.receipts_root,
        logs_bloom: header.logs_bloom,
        gas_used: header.gas_used,
        withdrawals_root: None,
        blob_gas_used: None,
        requests_hash: header.requests_hash,
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

    assert_eq!(err, PrecomputedOutcomeValidationError::RequestsHashUnexpected);
}

#[test]
fn canonicalize_accepts_withdrawals_root_when_shanghai_active() {
    let schedule = TestSchedule;
    let mut header = test_header();
    header.timestamp = 15;
    header.state_root = B256::from([0x11; 32]);
    header.withdrawals_root = Some(B256::from([0x22; 32]));

    let inputs = ComparisonInputs {
        tx_root: header.transactions_root,
        receipts_root: header.receipts_root,
        logs_bloom: header.logs_bloom,
        gas_used: header.gas_used,
        withdrawals_root: header.withdrawals_root,
        blob_gas_used: None,
        requests_hash: None,
    };

    canonicalize_and_compare(
        &header,
        &inputs,
        header.state_root,
        header.number,
        header.timestamp,
        header.extra_data.as_ref(),
        ParentHeaderView { timestamp: 0, blob_gas_used: None, excess_blob_gas: None, base_fee_per_gas: 0 },
        &schedule,
    )
    .expect("withdrawals_root is required and compared once shanghai is active");
}

#[test]
fn canonicalize_accepts_requests_hash_when_prague_active() {
    let schedule = TestSchedule;
    let mut header = test_header();
    header.timestamp = 35;
    header.number = 42;
    header.state_root = B256::from([0x11; 32]);
    header.withdrawals_root = Some(B256::from([0x22; 32]));
    header.blob_gas_used = Some(0);
    header.excess_blob_gas = Some(schedule.blob_params_at(header.number, header.timestamp).next_block_excess_blob_gas_osaka(0, 0, 0));
    header.requests_hash = Some(B256::from([0x33; 32]));

    let inputs = ComparisonInputs {
        tx_root: header.transactions_root,
        receipts_root: header.receipts_root,
        logs_bloom: header.logs_bloom,
        gas_used: header.gas_used,
        withdrawals_root: header.withdrawals_root,
        blob_gas_used: header.blob_gas_used,
        requests_hash: header.requests_hash,
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
    .expect("requests_hash is required and compared once prague is active");
}
