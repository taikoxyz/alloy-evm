use super::*;

fn tx_env(tx_type: u8, gas_price: u128, gas_priority_fee: Option<u128>) -> TxEnv {
    let mut tx = TxEnv::default();
    tx.tx_type = tx_type;
    tx.gas_price = gas_price;
    tx.gas_priority_fee = gas_priority_fee;
    tx
}

#[test]
fn tx_fee_fields_mapping_is_locked_by_tx_type() {
    assert_eq!(
        tx_fee_fields_from_tx(&tx_env(TransactionType::Legacy as u8, 21, None)),
        Ok(TxFeeFields { max_fee_per_gas: 21, max_priority_fee_per_gas: 21 })
    );

    assert_eq!(
        tx_fee_fields_from_tx(&tx_env(TransactionType::Eip2930 as u8, 22, None)),
        Ok(TxFeeFields { max_fee_per_gas: 22, max_priority_fee_per_gas: 22 })
    );

    assert_eq!(
        tx_fee_fields_from_tx(&tx_env(TransactionType::Eip1559 as u8, 100, Some(3))),
        Ok(TxFeeFields { max_fee_per_gas: 100, max_priority_fee_per_gas: 3 })
    );

    assert_eq!(
        tx_fee_fields_from_tx(&tx_env(TransactionType::Eip4844 as u8, 120, Some(5))),
        Ok(TxFeeFields { max_fee_per_gas: 120, max_priority_fee_per_gas: 5 })
    );
}

#[test]
fn tx_fee_fields_missing_or_unsupported_fail_closed() {
    let missing_priority =
        tx_fee_fields_from_tx(&tx_env(TransactionType::Eip1559 as u8, 30, None))
            .expect_err("missing priority must fail");
    assert_eq!(
        missing_priority,
        FeeError::MissingTxFeeFields {
            tx_type: TransactionType::Eip1559 as u8,
            field: "gas_priority_fee",
        }
    );

    let unsupported = tx_fee_fields_from_tx(&tx_env(TransactionType::Eip7702 as u8, 30, Some(1)))
        .expect_err("unsupported must fail");
    assert_eq!(unsupported, FeeError::UnsupportedTxType { tx_type: TransactionType::Eip7702 as u8 });
}

#[test]
fn compute_multichain_fees_matches_sigma_contract() {
    let gas_used_per_chain =
        HashMap::from_iter([(1u64, 100u64), (2u64, 50u64), (3u64, 0u64)]);
    let per_chain_basefee =
        HashMap::from_iter([(1u64, 10u64), (2u64, 20u64), (3u64, 999u64)]);
    let tx_fee_fields = TxFeeFields { max_fee_per_gas: 50, max_priority_fee_per_gas: 15 };

    let fees = compute_multichain_fees(&gas_used_per_chain, &per_chain_basefee, tx_fee_fields)
        .unwrap_or_else(|err| panic!("sigma fees mapping failed: {err:?}"));

    // chain 1: basefee=10, tip=min(15, 40)=15, gas=100
    // chain 2: basefee=20, tip=min(15, 30)=15, gas=50
    assert_eq!(fees.gas_used_total, 150);
    assert_eq!(fees.total_basefee_burn, U256::from(2_000u64));
    assert_eq!(fees.total_tip, U256::from(2_250u64));
    assert_eq!(fees.total_fee, U256::from(4_250u64));
}

#[test]
fn compute_multichain_fees_fails_when_basefee_is_missing_or_invalid() {
    let tx_fee_fields = TxFeeFields { max_fee_per_gas: 30, max_priority_fee_per_gas: 10 };

    let missing = compute_multichain_fees(
        &HashMap::from_iter([(1u64, 100u64), (2u64, 1u64)]),
        &HashMap::from_iter([(1u64, 10u64)]),
        tx_fee_fields,
    )
    .expect_err("missing chain basefee must fail");
    assert_eq!(missing, FeeError::MissingBasefee { chain_id: 2, gas_used: 1 });

    let invalid = compute_multichain_fees(
        &HashMap::from_iter([(1u64, 1u64)]),
        &HashMap::from_iter([(1u64, 31u64)]),
        tx_fee_fields,
    )
    .expect_err("basefee > max fee must fail");
    assert_eq!(
        invalid,
        FeeError::BasefeeExceedsMaxFee { chain_id: 1, basefee: 31, max_fee_per_gas: 30 }
    );
}

#[test]
fn phase62_3_charged_chain_insertion_order_deterministic_errors() {
    let tx_fee_fields = TxFeeFields { max_fee_per_gas: 30, max_priority_fee_per_gas: 10 };

    let insertion_orders = [
        vec![(7u64, 1u64), (3u64, 1u64), (5u64, 1u64)],
        vec![(5u64, 1u64), (7u64, 1u64), (3u64, 1u64)],
        vec![(3u64, 1u64), (5u64, 1u64), (7u64, 1u64)],
    ];

    let mut first_missing_basefee: Option<FeeError> = None;
    for order in &insertion_orders {
        let gas_used_per_chain = HashMap::from_iter(order.iter().copied());
        let err = compute_multichain_fees(
            &gas_used_per_chain,
            &HashMap::from_iter([(5u64, 10u64)]),
            tx_fee_fields,
        )
        .expect_err("missing basefee must fail deterministically");

        if let Some(expected) = &first_missing_basefee {
            assert_eq!(&err, expected, "missing-basefee error drifted across insertion orders");
        } else {
            first_missing_basefee = Some(err.clone());
        }
    }
    assert_eq!(first_missing_basefee, Some(FeeError::MissingBasefee { chain_id: 3, gas_used: 1 }));

    let mut first_max_fee_error: Option<FeeError> = None;
    for order in &insertion_orders {
        let gas_used_per_chain = HashMap::from_iter(order.iter().copied());
        let err = compute_multichain_fees(
            &gas_used_per_chain,
            &HashMap::from_iter([(3u64, 31u64), (5u64, 10u64), (7u64, 32u64)]),
            tx_fee_fields,
        )
        .expect_err("basefee > max fee must fail deterministically");

        if let Some(expected) = &first_max_fee_error {
            assert_eq!(&err, expected, "max-fee error drifted across insertion orders");
        } else {
            first_max_fee_error = Some(err.clone());
        }
    }
    assert_eq!(
        first_max_fee_error,
        Some(FeeError::BasefeeExceedsMaxFee {
            chain_id: 3,
            basefee: 31,
            max_fee_per_gas: 30,
        })
    );
}

