//! Faber plugins, v1: the `faber:plugin@1.0.0` contract and the core's half
//! of it.
//!
//! A plugin defines tools, handles their calls, and may post notices; nothing
//! else is in v1. The normative rules are the Faber Plugin Spec v1 and the
//! contract is `wit/plugin.wit`. This crate holds what the core owns of them:
//!
//! - [`types`]: the WIT `types` interface, in Rust.
//! - [`manifest`]: the manifest format and its rules.
//! - [`Plugin`] and [`SessionCtx`]: the exports and the host interface, for
//!   built-ins implementing the contract natively.
//! - [`notice`]: the per-session notice queue, its delivery modes and limits.
//! - [`bind`]: bindings and what bind time refuses.
//! - [`run`]: the run-start lifecycle, the system head, and the dispatcher
//!   that routes `<plugin id>__<tool>` calls.
//!
//! Third-party components (Wasmtime, WASI 0.3) are not hosted yet; every type
//! here is the one a component host will fill from the generated bindings.

pub mod bind;
pub mod manifest;
pub mod notice;
mod plugin;
pub mod registry;
pub mod run;
pub mod schema;
pub mod types;

pub use bind::{BindError, Binding, Prepared};
pub use manifest::Manifest;
pub use notice::{Moment, Notices, Pending, RunNotices};
pub use plugin::{Plugin, SessionCtx};
pub use registry::Registry;
pub use run::{Degraded, Dispatcher, Snapshot, Started};
pub use types::*;

/// The contract this host implements, as its WIT package names it.
pub const CONTRACT: &str = "faber:plugin@1.0.0";

/// The WIT package, verbatim.
pub const WIT: &str = include_str!("../wit/plugin.wit");
