use super::{BoundedEvmFactory, Database};
use crate::eth::EthEvmFactory;
use revm::database_interface::EmptyDB;

fn assert_impl<F, DB>()
where
    F: BoundedEvmFactory<DB>,
    DB: Database,
{
}

#[test]
fn bounded_factory_blanket_impl_compiles_for_eth_factory() {
    assert_impl::<EthEvmFactory, EmptyDB>();
}

