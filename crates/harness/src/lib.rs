//! Runs a conversation's turn: the loop that calls the model, dispatches the
//! tools it asks for, and commits what the turn ended on.
//!
//! [`HarnessRun::start`] spawns the loop as a task. The loop reaches the
//! model, the tools and the history only through the [`Context`] it is
//! handed, and every outbound step is recorded in the frame log there.

mod canonical;
mod context;
mod conversational;
pub mod error;
pub mod frame;
pub mod interrupt;
pub mod mapping;
pub mod runtime;
mod scaffold;
pub mod state;
pub mod tools;
mod validate;

pub use context::{Call, Context};
pub use error::Error;
pub use interrupt::{Interrupt, Interrupter, interrupt};
pub use runtime::{HarnessRun, RunError, RunOutcome, Terminator};
pub use state::FunctionRegistry;
pub use state::{Baseline, Grant, Reasoning, Seed};
pub use tools::{Surface, Toolbox, web::Web};
