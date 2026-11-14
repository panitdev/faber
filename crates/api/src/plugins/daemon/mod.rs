//! The host side of plugin daemons and per-binding storage
//! ([`plugin::daemon`]).
//!
//! A binding is one plugin type in one project. Everything here is keyed by
//! that pair, taken from the handle the host gave out or from the session a
//! tool handler serves, never from what a plugin passes in.

pub mod sessions;
pub mod storage;

#[cfg(test)]
pub(crate) mod fixture;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use uuid::Uuid;

/// Which binding: a plugin type in a project.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingKey {
    pub project: Uuid,
    pub plugin: String,
}

impl BindingKey {
    pub fn new(project: Uuid, plugin: impl Into<String>) -> Self {
        BindingKey {
            project,
            plugin: plugin.into(),
        }
    }

    /// The opaque id a daemon sees: stable across config changes and
    /// restarts, because the pair is.
    pub fn id(&self) -> String {
        self.to_string()
    }

    /// The pair back from [`BindingKey::id`].
    pub fn parse(id: &str) -> Option<BindingKey> {
        let (project, plugin) = id.split_once('/')?;
        Some(BindingKey::new(project.parse().ok()?, plugin))
    }
}

impl fmt::Display for BindingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.project, self.plugin)
    }
}

/// Whether a handle is still current. A `changed` or `removed` event retires
/// the lease of the handle it replaces, and every call through that handle or
/// its store answers `gone` from then on. Clones share one lease.
#[derive(Clone, Debug, Default)]
pub struct Lease(Arc<AtomicBool>);

impl Lease {
    pub fn new() -> Self {
        Lease::default()
    }

    pub fn retire(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_retired(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_id_round_trips() {
        let key = BindingKey::new(Uuid::now_v7(), "discord");
        assert_eq!(BindingKey::parse(&key.id()), Some(key));
        assert_eq!(BindingKey::parse("nope"), None);
    }
}
