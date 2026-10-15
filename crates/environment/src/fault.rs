//! Failure classes distinct in the type, not in a string.
//!
//! Two classes reach this type, and the agent's correct repair differs for
//! each: [`Fault::Denied`] means the API refused deterministically — do not
//! retry, the request was malformed — and [`Fault::Unreachable`] means the
//! transport failed, so do not edit the command, the machine is gone.
//!
//! The third class is not here on purpose. A command that ran and terminated
//! is [`Exit`](crate::Exit), whatever its exit code, and a command that hung
//! is [`Outcome::TimedOut`](crate::Outcome). Collapsing either into an error
//! teaches the model to debug its shell script when the SSH tunnel dropped
//!.
//!
//! [`Denial`] is wider than the original four variants because it needs to be: a
//! malformed regex, an offset past the end, a nonexistent path, and a
//! genuinely empty match set are four different results and never share a
//! representation. Three of those four are refusals, and the original
//! `PathEscape | MissingCapability | Disabled | NotBound` has nowhere to put
//! them.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::exec::ProcId;
use crate::manifest::Capability;

/// Everything that is not a result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum Fault {
    /// The API refused. Deterministic — the same call fails the same way.
    #[error("denied: {0}")]
    Denied(Denial),

    /// The transport failed. Says nothing about the request.
    #[error("unreachable: {0}")]
    Unreachable(String),
}

impl Fault {
    /// True when retrying the identical call could plausibly succeed. Only
    /// [`Fault::Unreachable`] qualifies; a [`Denial`] is a fact about the
    /// request.
    pub fn transient(&self) -> bool {
        matches!(self, Fault::Unreachable(_))
    }
}

impl From<Denial> for Fault {
    fn from(denial: Denial) -> Self {
        Fault::Denied(denial)
    }
}

/// Why the API refused. Each variant names a distinct repair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "denial", rename_all = "snake_case")]
pub enum Denial {
    /// The path left the target's root, or never named a place inside it.
    #[error("`{path}` is outside the target's root: {reason}")]
    PathEscape {
        path: String,
        /// `Cow` so the common case is a literal and nothing allocates, while a
        /// serialized denial still round-trips.
        reason: Cow<'static, str>,
    },

    /// The manifest published at bind does not carry this capability:
    /// capability is manifest data, and this is the backstop for a model that
    /// ignored what it was told — never the way it finds out).
    #[error("this target does not support {0}")]
    MissingCapability(Capability),

    /// The binding exists but the operator has disabled the host.
    #[error("this target is disabled")]
    Disabled,

    /// No such label, or a label that has been tombstoned. Unbinding
    /// removes nothing; the label stays in history and answers this forever.
    #[error("`{label}` is not a bound target")]
    NotBound { label: String },

    /// The path does not exist — distinct from an empty file, an empty
    /// listing, and a refused query.
    #[error("`{path}` does not exist")]
    NotFound { path: String },

    /// The window starts past the end of the file, which is different from
    /// a window that yielded nothing because the file is empty.
    #[error("offset {offset} is past the end of `{path}` ({len} available)")]
    OutOfRange { path: String, offset: u64, len: u64 },

    /// Claude Code shipped a rejected pattern as "no
    /// files found", which reads to the model as a settled negative answer and
    /// ends the search. The diagnostic goes back verbatim.
    #[error("`{pattern}` is not a valid pattern: {reason}")]
    BadPattern { pattern: String, reason: String },

    /// The process handle is unknown or already reaped. Not `Unreachable`:
    /// the machine answered.
    #[error("no such process {0}")]
    NoSuchProcess(ProcId),

    /// The process was stopped by the target, not by the caller: scratch
    /// stops what is left running when it tears an idle sandbox down.
    #[error("process {0} was stopped by idle teardown; start it again")]
    Stopped(ProcId),

    /// The request could not be interpreted at all. `what` names the field so
    /// the repair is localized.
    #[error("{what}: {reason}")]
    Malformed {
        what: Cow<'static, str>,
        reason: String,
    },

    /// An edit's precondition did not hold — the anchor was absent, or matched
    /// more than once without `all`. Deterministic, hence a denial rather than
    /// a failed write.
    #[error("edit precondition failed on `{path}`: {reason}")]
    EditRefused { path: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denial_is_never_transient_and_unreachable_always_is() {
        assert!(!Fault::Denied(Denial::Disabled).transient());
        assert!(Fault::Unreachable("connection reset".into()).transient());
    }
}
