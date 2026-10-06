//! The plugin types this host knows, by id and version.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::manifest::{Manifest, ManifestError};
use crate::plugin::Plugin;

/// Registered plugin types. Ids are unique; one id may have several versions,
/// because a session keeps the version it last ran with until its next run
/// start (and `on-removed` runs the last bound version).
#[derive(Default, Clone)]
pub struct Registry {
    types: BTreeMap<String, BTreeMap<Version, Arc<dyn Plugin>>>,
    /// Bound to new projects by default.
    defaults: Vec<String>,
}

/// A semver, ordered by its numeric core and then by the presence of a
/// pre-release (a release sorts after its own pre-releases).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    core: (u64, u64, u64),
    release: bool,
    text: String,
}

impl Version {
    fn parse(text: &str) -> Version {
        let without_build = text.split_once('+').map_or(text, |(core, _)| core);
        let (core, pre) = match without_build.split_once('-') {
            Some((core, _)) => (core, true),
            None => (without_build, false),
        };
        let mut parts = core.split('.').map(|part| part.parse().unwrap_or(0));
        Version {
            core: (
                parts.next().unwrap_or(0),
                parts.next().unwrap_or(0),
                parts.next().unwrap_or(0),
            ),
            release: !pre,
            text: text.to_owned(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("invalid manifest: {}", list(.0))]
    Manifest(Vec<ManifestError>),
    #[error("`{0}` {1} is already registered")]
    Duplicate(String, String),
    #[error("the default config fails the plugin's own validate: {0}")]
    DefaultRefused(String),
}

fn list(errors: &[ManifestError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    /// Registers a plugin type version. `default` binds it to new projects.
    pub fn register(
        &mut self,
        plugin: Arc<dyn Plugin>,
        default: bool,
    ) -> Result<(), RegisterError> {
        let manifest = plugin.manifest().clone();
        manifest.check().map_err(RegisterError::Manifest)?;
        plugin
            .validate(manifest.config.default.clone())
            .map_err(|errors| {
                RegisterError::DefaultRefused(
                    errors
                        .iter()
                        .map(|error| format!("{} {}", error.path, error.message))
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            })?;

        let versions = self.types.entry(manifest.id.clone()).or_default();
        let version = Version::parse(&manifest.version);
        if versions.contains_key(&version) {
            return Err(RegisterError::Duplicate(manifest.id, manifest.version));
        }
        versions.insert(version, plugin);
        if default && !self.defaults.contains(&manifest.id) {
            self.defaults.push(manifest.id);
        }
        Ok(())
    }

    /// The newest version of a type.
    pub fn latest(&self, id: &str) -> Option<Arc<dyn Plugin>> {
        self.types
            .get(id)?
            .last_key_value()
            .map(|(_, plugin)| Arc::clone(plugin))
    }

    /// An exact version of a type.
    pub fn get(&self, id: &str, version: &str) -> Option<Arc<dyn Plugin>> {
        self.types
            .get(id)?
            .iter()
            .find(|(registered, _)| registered.text == version)
            .map(|(_, plugin)| Arc::clone(plugin))
    }

    /// Every type, newest version each, in id order.
    pub fn manifests(&self) -> Vec<Manifest> {
        self.types
            .values()
            .filter_map(|versions| versions.last_key_value())
            .map(|(_, plugin)| plugin.manifest().clone())
            .collect()
    }

    /// The types a new project gets, in registration order.
    pub fn defaults(&self) -> Vec<Arc<dyn Plugin>> {
        self.defaults
            .iter()
            .filter_map(|id| self.latest(id))
            .collect()
    }

    /// Which type exports an interface, if any registered one does.
    pub fn exporters(&self, interface: &str) -> Vec<String> {
        self.types
            .iter()
            .filter(|(_, versions)| {
                versions
                    .values()
                    .any(|plugin| plugin.exports().iter().any(|e| e == interface))
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_by_semver_not_text() {
        let mut versions = vec![
            Version::parse("1.10.0"),
            Version::parse("1.2.0"),
            Version::parse("1.10.0-rc.1"),
        ];
        versions.sort();
        let ordered: Vec<&str> = versions.iter().map(|v| v.text.as_str()).collect();
        assert_eq!(ordered, ["1.2.0", "1.10.0-rc.1", "1.10.0"]);
    }
}
