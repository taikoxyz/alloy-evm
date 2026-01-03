//! Inspector composition for `alloy-gwyneth-evm`.
//!
//! Gwyneth requires the `JournalInspector` to be active to enforce structural invariants and to
//! surface structured hard-failure details. Additional user-provided inspectors (tracers, etc.)
//! are supported and can be toggled via `Evm::set_inspector_enabled`.

use gwyneth_engine::{HardFailureDetails, HardFailureInspector, JournalInspector};
use revm::{
    inspector::Inspector,
    interpreter::{
        interpreter::EthInterpreter, CallInputs, CallOutcome, CreateInputs, CreateOutcome,
        Interpreter,
    },
    primitives::{Address, Log, U256},
};

/// Fused inspector: always-on Gwyneth `JournalInspector` + optional user inspector.
#[derive(Debug)]
pub struct GwynethInspector<I> {
    journal: JournalInspector,
    user: I,
    user_enabled: bool,
}

impl<I> GwynethInspector<I> {
    /// Create a new fused inspector.
    pub fn new(user: I, user_enabled: bool) -> Self {
        Self { journal: JournalInspector::new(), user, user_enabled }
    }

    /// Enable/disable the user inspector (gwyneth inspector remains active).
    pub fn set_user_enabled(&mut self, enabled: bool) {
        self.user_enabled = enabled;
    }

    /// Returns whether the user inspector is enabled.
    pub const fn user_enabled(&self) -> bool {
        self.user_enabled
    }

    /// Access the gwyneth journal inspector.
    pub const fn journal(&self) -> &JournalInspector {
        &self.journal
    }

    /// Mutable access to the gwyneth journal inspector.
    pub fn journal_mut(&mut self) -> &mut JournalInspector {
        &mut self.journal
    }

    /// Access the user inspector.
    pub const fn user(&self) -> &I {
        &self.user
    }

    /// Mutable access to the user inspector.
    pub fn user_mut(&mut self) -> &mut I {
        &mut self.user
    }
}

impl<I> HardFailureInspector for GwynethInspector<I> {
    fn take_hard_failure_details(&mut self) -> Option<HardFailureDetails> {
        self.journal.take_hard_failure_details()
    }

    fn reset_for_new_tx(&mut self) {
        self.journal.reset_for_new_tx();
    }
}

impl<CTX, I> Inspector<CTX> for GwynethInspector<I>
where
    JournalInspector: Inspector<CTX>,
    I: Inspector<CTX>,
{
    fn initialize_interp(&mut self, interp: &mut Interpreter<EthInterpreter>, context: &mut CTX) {
        self.journal.initialize_interp(interp, context);
        if self.user_enabled {
            self.user.initialize_interp(interp, context);
        }
    }

    fn step(&mut self, interp: &mut Interpreter<EthInterpreter>, context: &mut CTX) {
        self.journal.step(interp, context);
        if self.user_enabled {
            self.user.step(interp, context);
        }
    }

    fn step_end(&mut self, interp: &mut Interpreter<EthInterpreter>, context: &mut CTX) {
        self.journal.step_end(interp, context);
        if self.user_enabled {
            self.user.step_end(interp, context);
        }
    }

    fn log(&mut self, interp: &mut Interpreter<EthInterpreter>, context: &mut CTX, log: Log) {
        // Match the tuple inspector behavior: first inspector sees a clone.
        self.journal.log(interp, context, log.clone());
        if self.user_enabled {
            self.user.log(interp, context, log);
        }
    }

    fn call(&mut self, context: &mut CTX, inputs: &mut CallInputs) -> Option<CallOutcome> {
        self.journal
            .call(context, inputs)
            .or_else(|| self.user_enabled.then(|| self.user.call(context, inputs)).flatten())
    }

    fn call_end(&mut self, context: &mut CTX, inputs: &CallInputs, outcome: &mut CallOutcome) {
        self.journal.call_end(context, inputs, outcome);
        if self.user_enabled {
            self.user.call_end(context, inputs, outcome);
        }
    }

    fn create(&mut self, context: &mut CTX, inputs: &mut CreateInputs) -> Option<CreateOutcome> {
        self.journal
            .create(context, inputs)
            .or_else(|| self.user_enabled.then(|| self.user.create(context, inputs)).flatten())
    }

    fn create_end(
        &mut self,
        context: &mut CTX,
        inputs: &CreateInputs,
        outcome: &mut CreateOutcome,
    ) {
        self.journal.create_end(context, inputs, outcome);
        if self.user_enabled {
            self.user.create_end(context, inputs, outcome);
        }
    }

    fn selfdestruct(&mut self, contract: Address, target: Address, value: U256) {
        self.journal.selfdestruct(contract, target, value);
        if self.user_enabled {
            self.user.selfdestruct(contract, target, value);
        }
    }
}

