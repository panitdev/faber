//! What a target is, published once at bind.
//!
//! The manifest is the bind event's result: it sits at a fixed position in the
//! prefix, before turn one, and is never edited afterward. Re-deriving it
//! mid-run is prefix mutation, and discovery-by-failure is forbidden.
//! forbid.
//!
//! The agent may observe only these differences between targets: manifest
//! contents, the posture line, and nothing else. Path shape, failure classes,
//! exec semantics, truncation behavior, and cwd handling are identical across
//! `(transport, exec_mode)` by construction — anything else that varies is a
//! bug in the implementation rather than a judgment call.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::fault::Denial;
use crate::path::Root;
use crate::registry::Label;

/// Everything the agent is told about a target, and everything it gets.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// The bound label. The addressable thing is a label, never an id:
    /// which host, which container, and which trust posture were decided by
    /// the user at bind time.
    pub label: Label,

    pub os: String,
    pub arch: String,
    /// The shell every [`Exec`](crate::Exec) command string runs through.
    pub shell: String,
    pub root: Root,
    /// Where the agent starts: the target's `$HOME` when it lies inside
    /// [`Manifest::root`], and the root otherwise. Paths are absolute and no
    /// `cwd` persists, so this is a fact to build paths from, never a default
    /// anything resolves against.
    #[serde(default)]
    pub home: String,

    /// Probed tool versions — `{"git": "2.43.0", ...}`. Per-binary capability
    /// is manifest data, never tool presence: no harness ships a
    /// `git` tool or a `cargo` tool.
    pub tools: BTreeMap<String, String>,

    /// Which verbs this target answers. Frozen here; the tool schema does not
    /// change to match (schema is prefix bytes, bindings are data).
    pub capabilities: BTreeSet<Capability>,

    /// Derived from `(transport, exec_mode)`, never stored.
    pub scope: Scope,
    /// In the manifest specifically because it is otherwise discovered by a
    /// failed `curl`, which is discovery-by-failure in its most common form.
    pub network: Reachability,
    pub posture: Posture,

    /// Exported into every command so rc files can detect the agent and skip
    /// fancy prompts and themes, the way Cursor's `CURSOR_AGENT` does.
    pub agent_env: BTreeMap<String, String>,
    /// Whether login rc files were sourced at all. Without
    /// it, "my alias works in my terminal and not in yours" is an unfalsifiable
    /// bug report.
    pub login_shell_sourced: bool,

    /// When this target was probed. With N targets bound at different times,
    /// "bind time" is per target and their manifests have different ages
    ///
    pub probed_at: SystemTime,
}

impl Manifest {
    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// The backstop for a model that ignored what the manifest told it — never
    /// the way it finds out.
    pub fn require(&self, capability: Capability) -> Result<(), Denial> {
        if self.supports(capability) {
            Ok(())
        } else {
            Err(Denial::MissingCapability(capability))
        }
    }
}

/// A verb class a target may or may not answer.
///
/// Not a list of binaries — those are [`Manifest::tools`]. These are the
/// operations of [`Target`](crate::Target) itself, and a transport that cannot
/// do one of them (no PTY, hence no [`Capability::Stdin`]) says so at bind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Exec,
    /// `start`/`output`/`signal`. Over SSH these handles die with the
    /// connection.
    Background,
    /// `stdin` into a running process, over a pipe. Enough to feed a program
    /// that reads stdin; not enough for one that opens `/dev/tty` or checks
    /// `isatty` — that is [`Capability::Pty`].
    Stdin,
    /// [`Capability::Stdin`] against a terminal rather than a pipe, which is
    /// what stdin is actually wanted for: a password prompt, a REPL that
    /// line-edits, a program that suppresses its prompt when not on a tty.
    ///
    /// Separate from `Stdin` because splitting them is what keeps a target
    /// from promising a prompt it cannot answer — a capability discovered by a
    /// write that silently goes nowhere is discovery-by-failure, and the
    /// manifest is supposed to be authoritative.
    Pty,
    Read,
    Write,
    Edit,
    /// Multi-file patch sets, including rename.
    Patch,
    List,
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Capability::Exec => "exec",
            Capability::Background => "background processes",
            Capability::Stdin => "stdin to a running process",
            Capability::Pty => "an interactive terminal",
            Capability::Read => "read",
            Capability::Write => "write",
            Capability::Edit => "edit",
            Capability::Patch => "patch sets",
            Capability::List => "list",
        };
        f.write_str(name)
    }
}

/// What the target's root actually covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The root is a directory on a machine that carries much more. The
    /// boundary is the API's alone.
    Workspace,
    /// The root is the container's filesystem; the substrate carries the rest
    /// of the boundary.
    Container,
}

/// Whether the target can reach the network. Stated, not discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    Reachable,
    Unreachable,
    /// Honest when nothing probed it. Better than asserting either, because
    /// both assertions get believed.
    Unknown,
}

/// Whether removed capability is removed by the substrate or by this API's
/// convention alone.
///
/// The one difference the agent may observe besides manifest
/// contents, and a trust posture the *user* chose at bind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Posture {
    /// A container: escaping the root means escaping the substrate.
    Enforced,
    /// Direct on a host: this API refuses escape and nothing underneath
    /// does. A shell command that walks out of the root is not stopped.
    Conventional,
}
