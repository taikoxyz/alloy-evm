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

    // Keep charged-chain diagnostics deterministic regardless of HashMap insertion order.
    let mut charged_chain_ids: Vec<u64> = gas_used_per_chain
        .iter()
        .filter_map(|(&chain_id, &gas_used)| (gas_used > 0).then_some(chain_id))
        .collect();
    charged_chain_ids.sort_unstable();

    for chain_id in charged_chain_ids {
        let Some(&gas_used) = gas_used_per_chain.get(&chain_id) else {
            return Err(FeeError::ArithmeticOverflow("charged_chain_ids missing key"));
        };

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
#[path = "fees_test.rs"]
mod tests;
