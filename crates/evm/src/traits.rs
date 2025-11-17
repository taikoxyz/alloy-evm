//! EVM traits.

use alloc::boxed::Box;
use alloy_primitives::{Address, Log, B256, U256};
use core::{error::Error, fmt, fmt::Debug};
use revm::primitives::ChainAddress;
use revm::{
    context::{Block, DBErrorMarker, JournalTr},
    interpreter::{SStoreResult, StateLoad},
    primitives::{StorageKey, StorageValue},
    state::{Account, AccountInfo, Bytecode},
};

/// Erased error type.
#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub struct ErasedError(Box<dyn Error + Send + Sync + 'static>);

impl ErasedError {
    /// Creates a new [`ErasedError`].
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}

impl DBErrorMarker for ErasedError {}

/// Errors returned by [`EvmInternals`].
#[derive(Debug, thiserror::Error)]
pub enum EvmInternalsError {
    /// Database error.
    #[error(transparent)]
    Database(ErasedError),
}

impl EvmInternalsError {
    /// Creates a new [`EvmInternalsError::Database`]
    pub fn database(err: impl Error + Send + Sync + 'static) -> Self {
        Self::Database(ErasedError::new(err))
    }
}

/// dyn-compatible trait for accessing and modifying EVM internals, particularly the journal.
///
/// This trait provides an abstraction over journal operations without exposing
/// associated types, making it object-safe and suitable for dynamic dispatch.
trait EvmInternalsTr:
    revm::database_interface::MultiChainDatabase<Error = ErasedError> + Debug
{
    fn load_account(
        &mut self,
        address: Address,
    ) -> Result<StateLoad<&mut Account>, EvmInternalsError>;

    fn load_account_code(
        &mut self,
        address: Address,
    ) -> Result<StateLoad<&mut Account>, EvmInternalsError>;

    fn sload(
        &mut self,
        address: Address,
        key: StorageKey,
    ) -> Result<StateLoad<StorageValue>, EvmInternalsError>;

    fn touch_account(&mut self, address: Address);

    fn set_code(&mut self, address: Address, code: Bytecode);

    fn sstore(
        &mut self,
        address: Address,
        key: StorageKey,
        value: StorageValue,
    ) -> Result<StateLoad<SStoreResult>, EvmInternalsError>;

    fn log(&mut self, log: Log);
}

/// Helper internal struct for implementing [`EvmInternals`].
#[derive(Debug)]
struct EvmInternalsImpl<'a, T> {
    journal: &'a mut T,
    chain_id: u64,
}

impl<T> revm::database_interface::MultiChainDatabase for EvmInternalsImpl<'_, T>
where
    T: JournalTr<Database: revm::database_interface::MultiChainDatabase>,
    <T::Database as revm::database_interface::MultiChainDatabase>::Error: Send + Sync + 'static,
{
    type Error = ErasedError;

    fn basic_multi(&mut self, address: ChainAddress) -> Result<Option<AccountInfo>, Self::Error> {
        self.journal.db_mut().basic_multi(address).map_err(ErasedError::new)
    }

    fn code_by_hash_multi(
        &mut self,
        chain_id: u64,
        code_hash: B256,
    ) -> Result<Bytecode, Self::Error> {
        self.journal.db_mut().code_by_hash_multi(chain_id, code_hash).map_err(ErasedError::new)
    }

    fn storage_multi(
        &mut self,
        address: ChainAddress,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.journal.db_mut().storage_multi(address, index).map_err(ErasedError::new)
    }

    fn block_hash_multi(&mut self, chain_id: u64, number: u64) -> Result<B256, Self::Error> {
        self.journal.db_mut().block_hash_multi(chain_id, number).map_err(ErasedError::new)
    }
}

impl<T> EvmInternalsTr for EvmInternalsImpl<'_, T>
where
    T: JournalTr<Database: revm::database_interface::MultiChainDatabase> + Debug,
    <T::Database as revm::database_interface::MultiChainDatabase>::Error: Send + Sync + 'static,
{
    fn load_account(
        &mut self,
        address: Address,
    ) -> Result<StateLoad<&mut Account>, EvmInternalsError> {
        // Convert Address to ChainAddress using the active chain_id
        let chain_address = ChainAddress::new(self.chain_id, address);
        self.journal.load_account(chain_address).map_err(EvmInternalsError::database)
    }

    fn load_account_code(
        &mut self,
        address: Address,
    ) -> Result<StateLoad<&mut Account>, EvmInternalsError> {
        // Convert Address to ChainAddress using the active chain_id
        let chain_address = ChainAddress::new(self.chain_id, address);
        self.journal.load_account_code(chain_address).map_err(EvmInternalsError::database)
    }

    fn sload(
        &mut self,
        address: Address,
        key: StorageKey,
    ) -> Result<StateLoad<StorageValue>, EvmInternalsError> {
        // Convert Address to ChainAddress using the active chain_id
        let chain_address = ChainAddress::new(self.chain_id, address);
        self.journal.sload(chain_address, key).map_err(EvmInternalsError::database)
    }

    fn touch_account(&mut self, address: Address) {
        // Convert Address to ChainAddress using the active chain_id
        let chain_address = ChainAddress::new(self.chain_id, address);
        self.journal.touch_account(chain_address);
    }

    fn set_code(&mut self, address: Address, code: Bytecode) {
        // Convert Address to ChainAddress using the active chain_id
        let chain_address = ChainAddress::new(self.chain_id, address);
        self.journal.set_code(chain_address, code);
    }

    fn sstore(
        &mut self,
        address: Address,
        key: StorageKey,
        value: StorageValue,
    ) -> Result<StateLoad<SStoreResult>, EvmInternalsError> {
        // Convert Address to ChainAddress using the active chain_id
        let chain_address = ChainAddress::new(self.chain_id, address);
        self.journal.sstore(chain_address, key, value).map_err(EvmInternalsError::database)
    }

    fn log(&mut self, log: Log) {
        self.journal.log(log);
    }
}

/// Helper type exposing hooks into EVM and access to evm internal settings.
pub struct EvmInternals<'a> {
    internals: Box<dyn EvmInternalsTr + 'a>,
    block_env: &'a (dyn Block + 'a),
}

impl<'a> EvmInternals<'a> {
    /// Creates a new [`EvmInternals`] instance.
    pub fn new<T>(journal: &'a mut T, block_env: &'a dyn Block, chain_id: u64) -> Self
    where
        T: JournalTr<Database: revm::database_interface::MultiChainDatabase> + Debug,
        <T::Database as revm::database_interface::MultiChainDatabase>::Error: Send + Sync + 'static,
    {
        Self { internals: Box::new(EvmInternalsImpl { journal, chain_id }), block_env }
    }

    /// Returns the  evm's block information.
    pub const fn block_env(&self) -> impl Block + 'a {
        self.block_env
    }

    /// Returns the current block number.
    pub fn block_number(&self) -> U256 {
        self.block_env.number()
    }

    /// Returns the current block timestamp.
    pub fn block_timestamp(&self) -> U256 {
        self.block_env.timestamp()
    }

    /// Returns a mutable reference to [`MultiChainDatabase`] implementation with erased error type.
    ///
    /// Users should prefer using other methods for accessing state that rely on cached state in the
    /// journal instead.
    pub fn db_mut(
        &mut self,
    ) -> impl revm::database_interface::MultiChainDatabase<Error = ErasedError> + '_ {
        &mut *self.internals
    }

    /// Loads an account.
    pub fn load_account(
        &mut self,
        address: Address,
    ) -> Result<StateLoad<&mut Account>, EvmInternalsError> {
        self.internals.load_account(address)
    }

    /// Loads code of an account.
    pub fn load_account_code(
        &mut self,
        address: Address,
    ) -> Result<StateLoad<&mut Account>, EvmInternalsError> {
        self.internals.load_account_code(address)
    }

    /// Loads a storage slot.
    pub fn sload(
        &mut self,
        address: Address,
        key: StorageKey,
    ) -> Result<StateLoad<StorageValue>, EvmInternalsError> {
        self.internals.sload(address, key)
    }

    /// Touches the account.
    pub fn touch_account(&mut self, address: Address) {
        self.internals.touch_account(address);
    }

    /// Sets bytecode to the account.
    pub fn set_code(&mut self, address: Address, code: Bytecode) {
        self.internals.set_code(address, code);
    }

    /// Stores the storage value in Journal state.
    pub fn sstore(
        &mut self,
        address: Address,
        key: StorageKey,
        value: StorageValue,
    ) -> Result<StateLoad<SStoreResult>, EvmInternalsError> {
        self.internals.sstore(address, key, value)
    }

    /// Logs the log in Journal state.
    pub fn log(&mut self, log: Log) {
        self.internals.log(log);
    }
}

impl<'a> fmt::Debug for EvmInternals<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EvmInternals")
            .field("internals", &self.internals)
            .field("block_env", &"{{}}")
            .finish_non_exhaustive()
    }
}
