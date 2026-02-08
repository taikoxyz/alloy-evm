use alloy_primitives::{map::HashMap, U256};
use revm::{
    context::tx::TxEnv,
    context_interface::transaction::TransactionType,
};

/// Canonical tx fee fields used by multi-chain fee attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxFeeFields {
    /// Max total price per gas unit.
    pub max_fee_per_gas: u128,
    /// Max miner tip per gas unit.
    pub max_priority_fee_per_gas: u128,
}

/// Result of multi-chain fee attribution using per-chain gas/basefee inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultichainFees {
    /// Sum of EIP-1559 basefee burn across charged chains.
    pub total_basefee_burn: U256,
    /// Sum of effective tips across charged chains.
    pub total_tip: U256,
    /// Total charged fee (`total_basefee_burn + total_tip`).
    pub total_fee: U256,
    /// Total charged gas across all chains.
    pub gas_used_total: u64,
}

/// Fail-closed fee-attribution errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeeError {
    /// The tx type is unsupported by the locked Phase 60 fee-field mapping.
    UnsupportedTxType { tx_type: u8 },
    /// A required tx fee field is missing for the tx type.
    MissingTxFeeFields { tx_type: u8, field: &'static str },
    /// A charged chain has no configured basefee.
    MissingBasefee { chain_id: u64, gas_used: u64 },
    /// A charged chain has `basefee > max_fee_per_gas`, which is invalid.
    BasefeeExceedsMaxFee { chain_id: u64, basefee: u64, max_fee_per_gas: u128 },
    /// Checked arithmetic overflow/underflow.
    ArithmeticOverflow(&'static str),
}

/// Derive canonical fee fields from tx-type-locked inputs.
///
/// Mapping contract:
/// - Legacy + EIP-2930: `max_fee_per_gas = gas_price`, `max_priority_fee_per_gas = gas_price`
/// - EIP-1559 + EIP-4844: `max_fee_per_gas = gas_price`, `max_priority_fee_per_gas = gas_priority_fee`
/// - Others: fail closed
pub fn tx_fee_fields_from_tx(tx: &TxEnv) -> Result<TxFeeFields, FeeError> {
    let tx_type = tx.tx_type;
    match TransactionType::from(tx_type) {
        TransactionType::Legacy | TransactionType::Eip2930 => Ok(TxFeeFields {
            max_fee_per_gas: tx.gas_price,
            max_priority_fee_per_gas: tx.gas_price,
        }),
        TransactionType::Eip1559 | TransactionType::Eip4844 => {
            let Some(max_priority_fee_per_gas) = tx.gas_priority_fee else {
                return Err(FeeError::MissingTxFeeFields {
                    tx_type,
                    field: "gas_priority_fee",
                });
            };
            Ok(TxFeeFields { max_fee_per_gas: tx.gas_price, max_priority_fee_per_gas })
        }
        TransactionType::Eip7702 | TransactionType::Custom => {
            Err(FeeError::UnsupportedTxType { tx_type })
        }
    }
}

/// Compute canonical multi-chain fees from charged gas and per-chain basefees.
pub fn compute_multichain_fees(
    gas_used_per_chain: &HashMap<u64, u64>,
    per_chain_basefee: &HashMap<u64, u64>,
    tx_fee_fields: TxFeeFields,
) -> Result<MultichainFees, FeeError> {
    let mut total_basefee_burn = U256::ZERO;
    let mut total_tip = U256::ZERO;
    let mut gas_used_total = 0u64;

    for (&chain_id, &gas_used) in gas_used_per_chain {
        if gas_used == 0 {
            continue;
        }

        let Some(&basefee) = per_chain_basefee.get(&chain_id) else {
            return Err(FeeError::MissingBasefee { chain_id, gas_used });
        };

        let basefee_u128 = u128::from(basefee);
        if basefee_u128 > tx_fee_fields.max_fee_per_gas {
            return Err(FeeError::BasefeeExceedsMaxFee {
                chain_id,
                basefee,
                max_fee_per_gas: tx_fee_fields.max_fee_per_gas,
            });
        }

        let remaining_fee_headroom = tx_fee_fields
            .max_fee_per_gas
            .checked_sub(basefee_u128)
            .ok_or(FeeError::ArithmeticOverflow("max_fee_per_gas - basefee"))?;
        let effective_tip_per_gas =
            core::cmp::min(tx_fee_fields.max_priority_fee_per_gas, remaining_fee_headroom);

        let gas_u256 = U256::from(gas_used);
        let chain_basefee_burn = U256::from(basefee_u128)
            .checked_mul(gas_u256)
            .ok_or(FeeError::ArithmeticOverflow("basefee * gas_used"))?;
        let chain_tip = U256::from(effective_tip_per_gas)
            .checked_mul(gas_u256)
            .ok_or(FeeError::ArithmeticOverflow("tip_per_gas * gas_used"))?;

        total_basefee_burn = total_basefee_burn
            .checked_add(chain_basefee_burn)
            .ok_or(FeeError::ArithmeticOverflow("sum(total_basefee_burn)"))?;
        total_tip = total_tip
            .checked_add(chain_tip)
            .ok_or(FeeError::ArithmeticOverflow("sum(total_tip)"))?;
        gas_used_total = gas_used_total
            .checked_add(gas_used)
            .ok_or(FeeError::ArithmeticOverflow("sum(gas_used_total)"))?;
    }

    let total_fee = total_basefee_burn
        .checked_add(total_tip)
        .ok_or(FeeError::ArithmeticOverflow("total_basefee_burn + total_tip"))?;

    Ok(MultichainFees { total_basefee_burn, total_tip, total_fee, gas_used_total })
}

#[cfg(test)]
mod tests {
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
        let legacy = tx_fee_fields_from_tx(&tx_env(TransactionType::Legacy as u8, 21, None))
            .expect("legacy mapping");
        assert_eq!(
            legacy,
            TxFeeFields { max_fee_per_gas: 21, max_priority_fee_per_gas: 21 }
        );

        let eip2930 = tx_fee_fields_from_tx(&tx_env(TransactionType::Eip2930 as u8, 22, None))
            .expect("eip2930 mapping");
        assert_eq!(
            eip2930,
            TxFeeFields { max_fee_per_gas: 22, max_priority_fee_per_gas: 22 }
        );

        let eip1559 = tx_fee_fields_from_tx(&tx_env(TransactionType::Eip1559 as u8, 100, Some(3)))
            .expect("eip1559 mapping");
        assert_eq!(
            eip1559,
            TxFeeFields { max_fee_per_gas: 100, max_priority_fee_per_gas: 3 }
        );

        let eip4844 = tx_fee_fields_from_tx(&tx_env(TransactionType::Eip4844 as u8, 120, Some(5)))
            .expect("eip4844 mapping");
        assert_eq!(
            eip4844,
            TxFeeFields { max_fee_per_gas: 120, max_priority_fee_per_gas: 5 }
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

        let unsupported =
            tx_fee_fields_from_tx(&tx_env(TransactionType::Eip7702 as u8, 30, Some(1)))
                .expect_err("unsupported must fail");
        assert_eq!(
            unsupported,
            FeeError::UnsupportedTxType { tx_type: TransactionType::Eip7702 as u8 }
        );
    }

    #[test]
    fn compute_multichain_fees_matches_sigma_contract() {
        let gas_used_per_chain =
            HashMap::from_iter([(1u64, 100u64), (2u64, 50u64), (3u64, 0u64)]);
        let per_chain_basefee = HashMap::from_iter([(1u64, 10u64), (2u64, 20u64), (3u64, 999u64)]);
        let tx_fee_fields = TxFeeFields { max_fee_per_gas: 50, max_priority_fee_per_gas: 15 };

        let fees = compute_multichain_fees(&gas_used_per_chain, &per_chain_basefee, tx_fee_fields)
            .expect("sigma fees");

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
}
