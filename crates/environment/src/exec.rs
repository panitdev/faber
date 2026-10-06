//! Running commands, and what running one produces.
//!
//! The shape follows the common execution contract, with three amendments:
//!
//! - Every [`Exit`] echoes the resolved `cwd` and the target label, so the
//!   transcript carries the resolution rather than implying it.
//! - Captured output is both a [`Span`] (for the exchange) and a path under
//!   the target's root (for the agent, which can `rg` a path and cannot `rg`
//!   a ref).
//! - [`Target::stdin`](crate::Target::stdin) exists, because
//!   `start`/`output`/`signal` cannot answer an interactive prompt or drive a
//!   REPL.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::path::RootedPath;
use crate::registry::Label;
use crate::store::{Blob, Span};

/// A command to run. The shell binary comes from the manifest, never from the
/// call.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Exec {
    /// A shell string, run through the manifest's shell.
    pub command: String,

    /// Per-call, never persistent. `None` means the target's root.
    ///
    /// A persistent working directory is hidden state the transcript does not
    /// carry, so replay diverges for reasons nothing in the exchange explains
    /// — and it implies a long-lived shell, which is a lifecycle Faber would
    /// then own. Codex is the precedent for the stateless position:
    /// required `workdir` plus an explicit ban on `cd`.
    pub cwd: Option<RootedPath>,

    /// Additive overlay on the probed base environment.
    pub env: Vec<(String, String)>,

    pub stdin: Option<Blob>,

    /// A timeout produces [`Outcome::TimedOut`], not a [`Fault`](crate::Fault).
    pub timeout: Duration,
}

/// Long enough that a normal build finishes, short enough that a hung test
/// suite is reported rather than waited on. Callers override per call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

impl Exec {
    pub fn new(command: impl Into<String>) -> Self {
        Exec {
            command: command.into(),
            cwd: None,
            env: Vec::new(),
            stdin: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn cwd(mut self, cwd: RootedPath) -> Self {
        self.cwd = Some(cwd);
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn stdin(mut self, stdin: impl Into<Blob>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// A command that ran. Not a failure, whatever the exit code.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Exit {
    /// Which target, echoed rather than implied.
    pub target: Label,
    /// The *resolved* cwd, so a transcript replayed elsewhere sees the same
    /// directory the run saw.
    pub cwd: RootedPath,
    pub outcome: Outcome,
    pub stdout: Stream,
    pub stderr: Stream,
    pub wall: Duration,
}

/// How a command ended.
///
/// `TimedOut` is here rather than in [`Fault`](crate::Fault) because the
/// command ran. The agent needs to distinguish "your test suite hangs" from
/// "the host went away", and only one of those is about its code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Completed { code: i32 },
    TimedOut,
    Signaled { signal: Signal },
}

/// One captured stream.
///
/// `span` is what the exchange carries; `spill` is a path *inside the target*
/// holding the same bytes, present when capture was large enough that the
/// agent will want to grep rather than read. `truncated` is the flag a capped
/// result must carry so it never reads as a complete one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stream {
    pub span: Span,
    pub spill: Option<RootedPath>,
    pub truncated: bool,
}

impl Stream {
    pub fn new(span: Span) -> Self {
        let truncated = span.truncated;
        Stream {
            span,
            spill: None,
            truncated,
        }
    }

    pub fn spilled_to(mut self, path: RootedPath) -> Self {
        self.spill = Some(path);
        self
    }
}

/// A handle to a running process.
///
/// The lean approach stands: over SSH these die with the connection and the call
/// reports [`Fault::Unreachable`](crate::Fault::Unreachable), because
/// reattaching needs a host-side supervisor and that is adjacent to the
/// lifecycle ownership is intentionally absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProcId(pub u64);

impl fmt::Display for ProcId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "proc:{}", self.0)
    }
}

/// A background process, as `processes` lists it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Process {
    pub id: ProcId,
    pub command: String,
    /// The resolved cwd it was started in.
    pub cwd: RootedPath,
    pub started: std::time::SystemTime,
    /// `None` while it is still running.
    pub outcome: Option<Outcome>,
    /// Bytes produced so far — where a reader that wants only new output
    /// starts.
    pub stdout_len: u64,
    pub stderr_len: u64,
}

/// Where a reader left off, per stream. Byte offsets into what the process has
/// produced so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub stdout: u64,
    pub stderr: u64,
}

impl Cursor {
    /// From the beginning of both streams.
    pub const START: Cursor = Cursor {
        stdout: 0,
        stderr: 0,
    };
}

/// Output produced since a [`Cursor`], plus where to resume.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub target: Label,
    pub stdout: Span,
    pub stderr: Span,
    pub next: Cursor,
    /// `None` while the process is still running. Once set, the process has
    /// ended and further reads only drain what is left.
    pub outcome: Option<Outcome>,
}

/// Signals the agent may send. Process lifecycle is in scope; container
/// lifecycle is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Int,
    Term,
    Kill,
    Hup,
    Quit,
    Usr1,
    Usr2,
}

impl Signal {
    /// POSIX number, for transports that speak numbers.
    pub fn number(self) -> i32 {
        match self {
            Signal::Hup => 1,
            Signal::Int => 2,
            Signal::Quit => 3,
            Signal::Kill => 9,
            Signal::Usr1 => 10,
            Signal::Usr2 => 12,
            Signal::Term => 15,
        }
    }

    /// The inverse, for reading a wait status back.
    pub fn from_number(number: i32) -> Option<Signal> {
        Some(match number {
            1 => Signal::Hup,
            2 => Signal::Int,
            3 => Signal::Quit,
            9 => Signal::Kill,
            10 => Signal::Usr1,
            12 => Signal::Usr2,
            15 => Signal::Term,
            _ => return None,
        })
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Signal::Int => "SIGINT",
            Signal::Term => "SIGTERM",
            Signal::Kill => "SIGKILL",
            Signal::Hup => "SIGHUP",
            Signal::Quit => "SIGQUIT",
            Signal::Usr1 => "SIGUSR1",
            Signal::Usr2 => "SIGUSR2",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_numbers_round_trip() {
        for signal in [Signal::Int, Signal::Term, Signal::Kill, Signal::Hup] {
            assert_eq!(Signal::from_number(signal.number()), Some(signal));
        }
    }
}
