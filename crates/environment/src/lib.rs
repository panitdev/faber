//! Execution targets: reach a machine, then exec and edit files on it.
//!
//! Implements the internal environment contract and its tool-surface amendments.
//!
//! What this crate is *not*: the model-facing tool surface. The tools are a
//! projection of this contract and the projection is per-model —
//! Codex-family against `shell`/`apply_patch`, Claude-family against
//! `Bash`/`Read`/`Write`/`Edit`. The n+m factoring only pays off if the
//! projection is a formatting decision, so the JSON schemas, tool names, and
//! rendering policy live with the harness and never here.
//!
//! What this crate also is not: coupled to the harness. It depends on neither
//! `llm` nor `harness`, and names no run, session, or exchange.
//!
//! The load-bearing pieces:
//!
//! - [`RootedPath`] — one rooted namespace, enforced at the API boundary
//!   in *all* modes, including direct, where nothing underneath enforces it.
//! - [`Fault`] — two failure classes distinct in the type; a nonzero exit
//!   is an [`Outcome`], not a failure, and a timeout is an outcome too.
//! - [`Manifest`] — capability published once at bind, never re-derived.
//! - [`Registry`] — labels, append-only, tombstoning rather than removal.
//! - [`Target`] — the agent-facing contract. [`Machine`] is its only
//!   implementation; a mode is a `Machine` over a different [`Spawn`] and
//!   [`Files`], so adding one is mechanism and never behavior.

#[cfg(test)]
mod conformance;

pub mod docker;
pub mod exec;
pub mod fault;
pub mod file;
pub mod files;
pub mod local;
pub mod machine;
pub mod manifest;
pub mod path;
pub mod probe;
pub mod registry;
pub mod spawn;
pub mod ssh;
pub mod store;
pub mod target;

pub use docker::DockerTarget;
pub use exec::{Chunk, Cursor, Exec, Exit, Outcome, ProcId, Signal, Stream};
pub use fault::{Denial, Fault};
pub use file::{Edit, Entry, EntryKind, Listing, Patch, PatchOp, Replace, Stat, Window};
pub use files::{Confined, DirEntry, Files};
pub use local::LocalTarget;
pub use machine::Machine;
pub use manifest::{Capability, Manifest, Posture, Reachability, Scope};
pub use path::{Root, RootedPath};
pub use registry::{Binding, Label, Registry};
pub use spawn::{Proc, Run, Spawn};
pub use ssh::{HostKey, SshCredential, SshTarget};
pub use store::{Blob, BlobRef, Blobs, MemoryBlobs, Span};
pub use target::Target;
