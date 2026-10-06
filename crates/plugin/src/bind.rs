//! Bindings, and what bind time refuses.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::registry::Registry;
use crate::types::{ConfigError, Json};

/// Bytes of a binding's config, serialized.
pub const CONFIG_CAP: usize = 64 * 1024;

/// A plugin bound to a project, as the core sees it. At most one per type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    /// The manifest id.
    pub plugin: String,
    /// The plugin version bound; an upgrade changes it.
    pub version: String,
    /// The config version `config` was written under.
    pub config_version: u32,
    /// Opaque to the core.
    pub config: Json,
    pub enabled: bool,
    /// Creation order. Sets tool, notice and head order; only `open` runs in
    /// dependency order.
    pub order: i64,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BindError {
    #[error("no plugin type `{0}` is registered")]
    UnknownType(String),
    #[error("`{plugin}` has no registered version {version}")]
    UnknownVersion { plugin: String, version: String },
    #[error("config-invalid")]
    ConfigInvalid(Vec<ConfigError>),
    #[error("link-refused: {0}")]
    LinkRefused(String),
    #[error("dependency-cycle: {}", .0.join(" -> "))]
    DependencyCycle(Vec<String>),
    #[error(
        "`{interface}` is already provided by `{provider}`; a project has one provider per interface"
    )]
    DuplicateProvider { interface: String, provider: String },
    #[error("still imported by {}", .0.join(", "))]
    StillImported(Vec<String>),
}

impl BindError {
    /// The code the API reports, as the core doc names it.
    pub fn code(&self) -> &'static str {
        match self {
            BindError::UnknownType(_) | BindError::UnknownVersion { .. } => "unknown-plugin",
            BindError::ConfigInvalid(_) => "config-invalid",
            BindError::LinkRefused(_) => "link-refused",
            BindError::DependencyCycle(_) => "dependency-cycle",
            BindError::DuplicateProvider { .. } => "duplicate-provider",
            BindError::StillImported(_) => "still-imported",
        }
    }
}

/// A config ready to store: migrated to the manifest's config version and
/// normalized by the plugin's own `validate`.
#[derive(Clone, Debug, PartialEq)]
pub struct Prepared {
    pub version: String,
    pub config_version: u32,
    pub config: Json,
}

/// Runs `migrate` and `validate` on a config for `plugin` at `version` (the
/// newest when `None`). `config_version` is what the config was written
/// under; `None` means the manifest's current one.
pub fn prepare(
    registry: &Registry,
    plugin: &str,
    version: Option<&str>,
    config: Json,
    config_version: Option<u32>,
) -> Result<Prepared, BindError> {
    let found = match version {
        Some(version) => registry.get(plugin, version).ok_or_else(|| {
            if registry.latest(plugin).is_some() {
                BindError::UnknownVersion {
                    plugin: plugin.to_owned(),
                    version: version.to_owned(),
                }
            } else {
                BindError::UnknownType(plugin.to_owned())
            }
        })?,
        None => registry
            .latest(plugin)
            .ok_or_else(|| BindError::UnknownType(plugin.to_owned()))?,
    };
    let manifest = found.manifest();
    let current = manifest.config.version;
    let written = config_version.unwrap_or(current);

    let config = normalize(found.as_ref(), config, written).map_err(BindError::ConfigInvalid)?;

    Ok(Prepared {
        version: manifest.version.clone(),
        config_version: current,
        config,
    })
}

/// `migrate` when the config is older than the manifest's, then `validate`,
/// then the size cap. Run at bind time and again at every run start.
pub fn normalize(
    plugin: &dyn crate::Plugin,
    config: Json,
    written: u32,
) -> Result<Json, Vec<ConfigError>> {
    let current = plugin.manifest().config.version;
    if written > current {
        return Err(vec![ConfigError::whole(format!(
            "this config was written under config version {written}, \
             newer than this plugin's {current}"
        ))]);
    }
    let config = if written < current {
        plugin.migrate(config, written)?
    } else {
        config
    };
    let config = plugin.validate(config)?;
    let size = serde_json::to_vec(&config).map_or(0, |bytes| bytes.len());
    if size > CONFIG_CAP {
        return Err(vec![ConfigError::whole(format!(
            "the config is {size} bytes; the limit is {CONFIG_CAP}"
        ))]);
    }
    Ok(config)
}

/// The interface each binding provides, and refuses a second provider of one.
pub fn providers(
    registry: &Registry,
    bindings: &[Binding],
) -> Result<BTreeMap<String, String>, BindError> {
    let mut providers: BTreeMap<String, String> = BTreeMap::new();
    for binding in bindings {
        let Some(plugin) = registry.get(&binding.plugin, &binding.version) else {
            continue;
        };
        for interface in plugin.exports() {
            if let Some(existing) = providers.get(&interface)
                && existing != &binding.plugin
            {
                return Err(BindError::DuplicateProvider {
                    interface,
                    provider: existing.clone(),
                });
            }
            providers.insert(interface, binding.plugin.clone());
        }
    }
    Ok(providers)
}

/// Bind-time graph checks over the project's bindings as they would be after
/// the change: one provider per interface, and no import cycle.
pub fn check_graph(registry: &Registry, bindings: &[Binding]) -> Result<(), BindError> {
    let providers = providers(registry, bindings)?;
    let edges = edges(registry, bindings, &providers);

    // Depth-first, keeping the path so a cycle can be named.
    fn visit(
        node: &str,
        edges: &BTreeMap<String, Vec<String>>,
        path: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<(), BindError> {
        if let Some(start) = path.iter().position(|seen| seen == node) {
            let mut cycle = path[start..].to_vec();
            cycle.push(node.to_owned());
            return Err(BindError::DependencyCycle(cycle));
        }
        if done.contains(node) {
            return Ok(());
        }
        path.push(node.to_owned());
        for next in edges.get(node).into_iter().flatten() {
            visit(next, edges, path, done)?;
        }
        path.pop();
        done.insert(node.to_owned());
        Ok(())
    }

    let mut done = BTreeSet::new();
    for binding in bindings {
        visit(&binding.plugin, &edges, &mut Vec::new(), &mut done)?;
    }
    Ok(())
}

/// The bindings, other than `plugin`, that import an interface `plugin`
/// provides — what keeps it from being unbound.
pub fn importers(registry: &Registry, bindings: &[Binding], plugin: &str) -> Vec<String> {
    let Ok(providers) = providers(registry, bindings) else {
        return Vec::new();
    };
    bindings
        .iter()
        .filter(|binding| binding.plugin != plugin)
        .filter(|binding| {
            registry
                .get(&binding.plugin, &binding.version)
                .is_some_and(|found| {
                    found
                        .imports()
                        .iter()
                        .any(|interface| providers.get(interface).is_some_and(|p| p == plugin))
                })
        })
        .map(|binding| binding.plugin.clone())
        .collect()
}

/// importer → the providers it imports from.
pub(crate) fn edges(
    registry: &Registry,
    bindings: &[Binding],
    providers: &BTreeMap<String, String>,
) -> BTreeMap<String, Vec<String>> {
    let mut edges = BTreeMap::new();
    for binding in bindings {
        let Some(plugin) = registry.get(&binding.plugin, &binding.version) else {
            continue;
        };
        let targets: Vec<String> = plugin
            .imports()
            .iter()
            .filter_map(|interface| providers.get(interface).cloned())
            .collect();
        edges.insert(binding.plugin.clone(), targets);
    }
    edges
}
