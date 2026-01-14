use std::cell::Cell;

thread_local! {
    static TX_EXECUTION_ENABLED: Cell<bool> = const { Cell::new(false) };
    static TX_EXECUTION_COUNT: Cell<usize> = const { Cell::new(0) };
}

struct TxExecutionCounterGuard {
    prev_enabled: bool,
    prev_count: usize,
}

impl TxExecutionCounterGuard {
    fn start() -> Self {
        let prev_enabled = TX_EXECUTION_ENABLED.with(|enabled| enabled.replace(true));
        let prev_count = TX_EXECUTION_COUNT.with(|count| count.replace(0));
        Self { prev_enabled, prev_count }
    }

    fn count(&self) -> usize {
        TX_EXECUTION_COUNT.with(|count| count.get())
    }
}

impl Drop for TxExecutionCounterGuard {
    fn drop(&mut self) {
        TX_EXECUTION_ENABLED.with(|enabled| enabled.set(self.prev_enabled));
        TX_EXECUTION_COUNT.with(|count| count.set(self.prev_count));
    }
}

pub(crate) fn record_tx_execution() {
    TX_EXECUTION_ENABLED.with(|enabled| {
        if enabled.get() {
            TX_EXECUTION_COUNT.with(|count| count.set(count.get().saturating_add(1)));
        }
    });
}

/// Runs `f` with a thread-local transaction-execution counter enabled.
///
/// Returns the closure's result and the number of times a transaction was actually executed
/// (`execute_transaction_without_commit` was invoked) in the current thread.
pub fn with_tx_execution_counter<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let guard = TxExecutionCounterGuard::start();
    let result = f();
    let count = guard.count();
    (result, count)
}

