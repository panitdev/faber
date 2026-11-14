//! The host side of plugin daemons and per-binding storage
//! ([`plugin::daemon`]).
//!
//! A binding is one plugin type in one project. Everything here is keyed by
//! that pair, taken from the handle the host gave out or from the session a
//! tool handler serves, never from what a plugin passes in.

pub mod sessions;
pub mod storage;
pub mod supervisor;
pub mod websocket;

#[cfg(test)]
pub(crate) mod fixture;

/// Every enabled binding of a plugin version that runs a daemon, with its
/// config normalized as at run start. A binding whose config no longer
/// passes is left out (and logged): its daemon would refuse it anyway.
pub async fn desired(
    db: &crate::db::DbPool,
    registry: &plugin::Registry,
) -> Result<Vec<supervisor::Desired>, String> {
    use crate::schema::project_binding;
    use diesel::{ExpressionMethods, QueryDsl};
    use diesel_async::RunQueryDsl;

    let types: Vec<String> = registry
        .manifests()
        .into_iter()
        .filter(|manifest| manifest.capabilities.daemon.is_some())
        .map(|manifest| manifest.id)
        .collect();
    if types.is_empty() {
        return Ok(Vec::new());
    }
    let mut conn = db.get().await.map_err(|error| error.to_string())?;
    let rows: Vec<(Uuid, String, String, i32, plugin::Json)> = project_binding::table
        .filter(project_binding::enabled.eq(true))
        .filter(project_binding::plugin_type.eq_any(&types))
        .select((
            project_binding::project_id,
            project_binding::plugin_type,
            project_binding::version,
            project_binding::config_version,
            project_binding::config,
        ))
        .load(&mut conn)
        .await
        .map_err(|error| error.to_string())?;
    let mut desired = Vec::new();
    for (project, plugin_type, version, config_version, config) in rows {
        let Some(found) = registry.get(&plugin_type, &version) else {
            continue;
        };
        if found.daemon().is_none() {
            continue;
        }
        match plugin::bind::normalize(found.as_ref(), config, config_version.max(1) as u32) {
            Ok(config) => desired.push(supervisor::Desired {
                binding: BindingKey::new(project, plugin_type),
                version,
                config,
            }),
            Err(errors) => tracing::warn!(
                %project,
                plugin = %plugin_type,
                ?errors,
                "a daemon binding's config no longer passes; not served"
            ),
        }
    }
    Ok(desired)
}

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
