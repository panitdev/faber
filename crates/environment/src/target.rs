//! The internal contract every transport implements.
//!
//! Async and object-safe on purpose. SSH and docker are the reason this
//! abstraction exists, and a [`Registry`](crate::Registry) addressing labels
//! holds targets as `dyn Target` — so the trait is written through
//! `#[async_trait]` rather than with native async fns, which are not
//! `dyn`-compatible.
//!
//! **The design target is that the same transcript, replayed against a
//! different target of the same capability class, produces the same calls**
//! Not the same results — the same calls. Anything that makes the agent
//! write mode-specific commands is a defect in an implementation of this
//! trait.
//!
//! Three families of verb are absent by decision, not omission:
//!
//! - **No health or status verb**. The agent would gate on it and
//!   then blame a stale answer for a failure the attempt would have reported
//!   correctly. The authoritative answer is the call.
//! - **No lifecycle verb**. No create, start, stop, remove, or restart of
//!   a host or container. Process lifecycle is in scope; container lifecycle is
//!   not, and an agent that can restart its own container can destroy the state
//!   a user is mid-way through inspecting.
//! - **No bind or discover verb**. The agent cannot bind, and
//!   enumeration reaches bound labels only, never the host.

use async_trait::async_trait;

use crate::exec::{Chunk, Cursor, Exec, Exit, ProcId, Signal};
use crate::fault::Fault;
use crate::file::{Edit, Listing, Stat, Window};
use crate::manifest::Manifest;
use crate::path::RootedPath;
use crate::store::{Blob, Span};

#[async_trait]
pub trait Target: Send + Sync {
    /// Published at bind, frozen for the life of the binding.
    fn manifest(&self) -> &Manifest;

    /// Runs a command to completion, or to its timeout.
    ///
    /// A nonzero exit code is a **success** at this layer, and so is a
    /// timeout ([`Outcome::TimedOut`](crate::Outcome)). Only a refusal or a
    /// dead transport is a [`Fault`].
    async fn exec(&self, req: Exec) -> Result<Exit, Fault>;

    /// Starts a command and returns a handle without waiting for it.
    async fn start(&self, req: Exec) -> Result<ProcId, Fault>;

    /// Reads output produced since `from`.
    async fn output(&self, id: ProcId, from: Cursor) -> Result<Chunk, Fault>;

    /// Writes to a running process's stdin.
    ///
    /// The missing verb: `start`/`output`/`signal` cannot answer an
    /// interactive prompt or drive a REPL. Requires a PTY rather than a pipe
    /// on the transport, which is why it is a separate
    /// [`Capability`](crate::Capability).
    async fn stdin(&self, id: ProcId, body: &Blob) -> Result<(), Fault>;

    /// Signals a running process. Process lifecycle only.
    async fn signal(&self, id: ProcId, signal: Signal) -> Result<(), Fault>;

    /// Reads a file, or a line window of one.
    ///
    /// A missing path, a window past the end, and an empty file
    /// are three results and never share a representation.
    async fn read(&self, path: &RootedPath, window: Option<Window>) -> Result<Span, Fault>;

    /// Writes a file whole, creating it if absent.
    async fn write(&self, path: &RootedPath, body: &Blob) -> Result<Stat, Fault>;

    /// Applies an edit. Wider than the original `edit(path, Edit)` — the
    /// [`Edit`] carries its own paths, because a rename and a cross-file apply
    /// cannot be expressed against one.
    async fn edit(&self, edit: &Edit) -> Result<Vec<Stat>, Fault>;

    /// Lists a directory, optionally filtered by a glob.
    ///
    /// Non-recursive by default and glob evaluation Faber-side:
    /// host-side is fast and image-dependent, which is the risk this crate
    /// exists to remove.
    async fn list(&self, path: &RootedPath, glob: Option<&str>) -> Result<Listing, Fault>;

    /// Resolves an agent-supplied path against this target's root.
    ///
    /// Provided rather than required: escape refusal is identical in every
    /// mode by construction, and an implementation that could override it is
    /// an implementation that could differ from the others.
    fn path(&self, path: &str) -> Result<RootedPath, Fault> {
        RootedPath::new(&self.manifest().root, path).map_err(Fault::from)
    }

    /// This target's root, the default `cwd` for an [`Exec`].
    fn root(&self) -> RootedPath {
        RootedPath::root_of(&self.manifest().root)
    }
}
