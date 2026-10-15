//! Execution inside a container, over the Engine API.
//!
//! This module is one exec mode and *both* of its transports. `local+docker`
//! and `ssh+docker` differ only in which [`Daemon`] they are handed: the
//! difference is which socket gets dialled, and everything above that — exec,
//! archive, framing, signals — is written once. A second transport is a second
//! `Daemon`, not a second copy of this.
//!
//! What a container buys, and what it therefore publishes:
//!
//! - [`Scope::Container`] — the root is the container's filesystem rather than
//!   a directory on a machine carrying much more.
//! - [`Posture::Enforced`] — the substrate holds the boundary. Unlike direct
//!   execution, a shell command that walks out of the root does not get out,
//!   so the refusal is backed rather than conventional.
//! - [`Capability::Pty`] is still not advertised. The Engine API offers a TTY
//!   and it would work, but a TTY exec stops multiplexing its output, so
//!   stdout and stderr arrive merged — and quietly folding stderr into stdout
//!   is a worse failure than not offering the capability.
//!
//! Container lifecycle stays absent **from the bound target**, exactly as
//! elsewhere. [`DockerTarget`] exposes no start, stop, restart, or remove, and
//! a stopped container is reported rather than started: it may be stopped
//! deliberately, and an agent that can restart its own container can destroy
//! what a user is mid-way through inspecting. File verbs still work against a
//! stopped container, because the archive endpoints do not need it running —
//! so a stopped container degrades to a readable one rather than to nothing.
//!
//! [`engine::container_create`] and its siblings do exist, one level below,
//! and nothing on the target surface can reach them. They are there for the
//! consumer that creates a container because a *user* asked it to. The rule
//! was never "no container is ever created"; it is that the agent does not get
//! to decide what exists, only what happens inside what does.

pub mod daemon;
pub mod engine;
pub mod files;
pub mod http;
pub mod spawn;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::SystemTime;

pub use daemon::{Daemon, LocalSocket};
pub use files::DockerFiles;
pub use spawn::DockerSpawn;

use crate::fault::Fault;
use crate::machine::Machine;
use crate::manifest::{Capability, Manifest, Posture, Reachability, Scope};
use crate::path::Root;
use crate::probe::{SHELL, home_within, probe};
use crate::registry::Label;
use crate::store::Blobs;

/// Execution inside a registered container.
///
/// A constructor, not a type — like every mode, what it returns is a
/// [`Machine`].
pub struct DockerTarget;

impl DockerTarget {
    /// Probes and binds.
    ///
    /// `container` is a name or an id and is resolved by the daemon on every
    /// call rather than pinned here: a registration asserts that Faber knows
    /// the ref, never that the ref currently resolves, and the authoritative
    /// answer to whether it does is the next call.
    pub async fn bind(
        label: impl Into<Label>,
        daemon: Arc<dyn Daemon>,
        container: impl Into<String>,
        root: Root,
        blobs: Arc<dyn Blobs>,
    ) -> Result<Machine, Fault> {
        let label = label.into();
        let container = container.into();

        let spawn = Arc::new(DockerSpawn::new(
            Arc::clone(&daemon),
            container.clone(),
            root.as_str(),
        ));
        let files = Arc::new(DockerFiles::new(Arc::clone(&daemon), container.clone()));

        let probed = probe(spawn.as_ref(), root.as_str().to_owned()).await?;

        let capabilities = BTreeSet::from([
            Capability::Exec,
            Capability::Background,
            Capability::Stdin,
            Capability::Read,
            Capability::Write,
            Capability::Edit,
            Capability::Patch,
            Capability::List,
        ]);

        let agent_env = BTreeMap::from([
            ("FABER_AGENT".to_owned(), "1".to_owned()),
            ("FABER_TARGET".to_owned(), label.0.clone()),
        ]);

        let manifest = Manifest {
            label,
            os: probed.os,
            arch: probed.arch,
            shell: SHELL.to_owned(),
            home: home_within(&root, probed.home.as_deref()),
            root,
            tools: probed.tools,
            capabilities,
            // The root is the container's filesystem; the substrate carries
            // the rest of the boundary.
            scope: Scope::Container,
            // Whether this container has a network depends on how the user ran
            // it, and nothing here measured it. Asserting either way gets
            // believed.
            network: Reachability::Unknown,
            // A container: escaping the root means escaping the substrate.
            posture: Posture::Enforced,
            agent_env,
            login_shell_sourced: false,
            probed_at: SystemTime::now(),
        };

        Ok(Machine::new(manifest, spawn, files, blobs))
    }
}
