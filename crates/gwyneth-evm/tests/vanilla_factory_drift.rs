//! Compile-only drift guards for Phase 40 (factory expressiveness).

use alloy_evm::evm::BoundedEvmFactory;
use alloy_evm::eth::EthEvmFactory;
use alloy_evm::Database;
use revm::database_interface::EmptyDB;

fn assert_bounded_factory_compiles<F, DB>()
where
    F: BoundedEvmFactory<DB>,
    DB: Database,
{
}

#[test]
fn bounded_factory_blanket_impl_compiles_for_eth_factory() {
    assert_bounded_factory_compiles::<EthEvmFactory, EmptyDB>();
}

