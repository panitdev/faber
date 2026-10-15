//! `faber:environments@1.0.0` (`wit/environments.wit`): the interfaces the
//! Environments plugin and mounting plugins share, in Rust.
//!
//! Two directions. `mounts` is exported by Environments and imported by any
//! plugin that wants a live view in its session's scratch. `provider` is
//! exported by each such plugin and serves the files of its mounts — but it is
//! never imported: the core routes provider calls to the plugin that owns the
//! mount, so Environments keeps no import edge to every plugin that mounts,
//! and many plugins can export `provider` in one project without the
//! one-provider-per-interface rule getting in the way.
//!
//! Only built-ins can export `provider` for now. Their implementation is a
//! native [`Provider`] handed out by [`crate::Plugin::provider`]; a component
//! exporting it is refused by [`check_component_exports`].

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::bind::Binding;
use crate::notice::Notices;
use crate::plugin::SessionCtx;
use crate::registry::Registry;

/// The WIT package, verbatim.
pub const WIT: &str = include_str!("../wit/environments.wit");

/// Writes into an environment, as any caller does. Exported by Environments.
pub const HANDLE: &str = "faber:environments/handle";
/// Mount declarations in a session's scratch. Exported by Environments.
pub const MOUNTS: &str = "faber:environments/mounts";
/// Serves a plugin's mounts. Exported by each mounting plugin, routed by the
/// core and never imported.
pub const PROVIDER: &str = "faber:environments/provider";

/// Interfaces any number of bindings in one project may export, because the
/// core routes them by mount owner rather than linking one importer to one
/// provider.
pub const ROUTED: &[&str] = &[PROVIDER];

/// Whether an export is routed by the core rather than linked to importers.
pub fn is_routed(interface: &str) -> bool {
    ROUTED.contains(&interface)
}

/// Where Environments puts every mount: `/mnt/<plugin-id>/<name>`.
pub const MOUNT_ROOT: &str = "/mnt";

/// The scratch path of a plugin's mount.
pub fn mount_path(plugin: &str, name: &str) -> String {
    format!("{MOUNT_ROOT}/{plugin}/{name}")
}

/// A component's exports, checked at link time. A component may not export
/// `provider` yet: provider calls run inside FUSE and file tools with no
/// component instance behind them, and the routing for that is built-in only.
pub fn check_component_exports(exports: &[String]) -> Result<(), String> {
    match exports.iter().find(|export| export.as_str() == PROVIDER) {
        Some(_) => Err(format!(
            "`{PROVIDER}` can only be exported by built-in plugins for now; \
             a third-party plugin cannot serve mounts yet"
        )),
        None => Ok(()),
    }
}

// --- mounts ---

/// `record mount-spec`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct MountSpec {
    /// Unique per plugin; mounting the same name again replaces it.
    pub name: String,
    pub read_only: bool,
}

/// `enum mount-error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "kebab-case")]
pub enum MountError {
    /// The project has no scratch entry.
    #[error("this project has no scratch environment")]
    NoScratch,
    /// The project has one, turned off.
    #[error("scratch is turned off for this project")]
    ScratchDisabled,
    #[error(
        "a mount name is 1 to 64 letters, digits, `.`, `_` or `-`, starting with a letter or digit"
    )]
    InvalidName,
    #[error("no such mount")]
    NotFound,
    #[error("this session has as many mounts as it may hold")]
    TooMany,
}

/// `interface mounts`, host side: what a built-in that mounts is handed and
/// calls. The caller is the binding `ctx` belongs to, never an argument, so a
/// plugin can only ever touch its own mounts.
#[async_trait]
pub trait Mounts: Send + Sync + 'static {
    /// Mounts at `/mnt/<plugin-id>/<name>` and returns that path.
    async fn mount(&self, ctx: &SessionCtx, spec: MountSpec) -> Result<String, MountError>;
    async fn unmount(&self, ctx: &SessionCtx, name: &str) -> Result<(), MountError>;
}

// --- provider ---

/// `enum kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    File,
    Dir,
}

/// `record entry`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Entry {
    /// The last path segment; `""` for a mount's root.
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub mtime: u64,
    /// Opaque; changes whenever the content does. What `if-version` compares
    /// against.
    pub version: String,
}

/// `enum fs-error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "kebab-case")]
pub enum FsError {
    #[error("not found")]
    NotFound,
    #[error("already exists")]
    Exists,
    #[error("not a directory")]
    NotADirectory,
    #[error("is a directory")]
    IsADirectory,
    #[error("directory not empty")]
    NotEmpty,
    #[error("read-only")]
    ReadOnly,
    #[error("changed since it was read")]
    Conflict,
    #[error("too large")]
    TooLarge,
    #[error("permission denied")]
    Denied,
    #[error("the plugin's upstream failed")]
    Upstream,
}

impl FsError {
    /// The errno FUSE reports for it. FUSE can only say this much, which is
    /// why the real error is also logged where the agent can read it.
    pub fn errno(self) -> i32 {
        // Linux values; FUSE in the sandbox is Linux.
        match self {
            FsError::NotFound => 2,       // ENOENT
            FsError::Exists => 17,        // EEXIST
            FsError::NotADirectory => 20, // ENOTDIR
            FsError::IsADirectory => 21,  // EISDIR
            FsError::NotEmpty => 39,      // ENOTEMPTY
            FsError::ReadOnly => 30,      // EROFS
            FsError::Conflict => 116,     // ESTALE: the file changed under the writer
            FsError::TooLarge => 27,      // EFBIG
            FsError::Denied => 13,        // EACCES
            FsError::Upstream => 5,       // EIO
        }
    }
}

/// `interface provider`, for built-ins. Seven operations cover `read`,
/// `patch` and a filesystem layer under exec; writes replace whole files.
///
/// `mount` is the mount's name as the plugin declared it; `path` is absolute
/// within the mount (`/` is its root) and already normalized by the core.
/// Every call takes the session's context, so a provider may serve different
/// content per session or per user.
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    async fn stat(&self, ctx: &SessionCtx, mount: &str, path: &str) -> Result<Entry, FsError>;

    async fn read_dir(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        path: &str,
    ) -> Result<Vec<Entry>, FsError>;

    async fn read(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        path: &str,
        offset: u64,
        max: u32,
    ) -> Result<Vec<u8>, FsError>;

    /// Whole-file write: create, replace, or fail with `conflict` when
    /// `if_version` no longer matches. `create_only` fails with `exists` when
    /// the file is already there.
    async fn put(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        path: &str,
        data: Vec<u8>,
        if_version: Option<String>,
        create_only: bool,
    ) -> Result<Entry, FsError>;

    async fn mkdir(&self, ctx: &SessionCtx, mount: &str, path: &str) -> Result<Entry, FsError>;

    async fn rename(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        src: &str,
        dst: &str,
    ) -> Result<(), FsError>;

    async fn remove(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), FsError>;
}

/// A shared provider handle, as [`crate::Plugin::provider`] returns it.
pub type SharedProvider = Arc<dyn Provider>;

/// The routing table for `provider`: which enabled binding of a project
/// serves mounts, and through what. Built from the project's bindings, like
/// a run's dispatcher, so a call reaches the version the project has bound
/// and a plugin removed or turned off is simply absent.
#[derive(Clone, Default)]
pub struct ProviderRoutes {
    routes: BTreeMap<String, ProviderRoute>,
}

#[derive(Clone)]
struct ProviderRoute {
    provider: SharedProvider,
    order: i64,
}

impl ProviderRoutes {
    pub fn new(registry: &Registry, bindings: &[Binding]) -> Self {
        let routes = bindings
            .iter()
            .filter(|binding| binding.enabled)
            .filter_map(|binding| {
                let plugin = registry.get(&binding.plugin, &binding.version)?;
                if !plugin.exports().iter().any(|e| e == PROVIDER) {
                    return None;
                }
                let provider = plugin.provider()?;
                Some((
                    binding.plugin.clone(),
                    ProviderRoute {
                        provider,
                        order: binding.order,
                    },
                ))
            })
            .collect();
        ProviderRoutes { routes }
    }

    /// Whether `plugin` is bound, enabled and serves mounts.
    pub fn serves(&self, plugin: &str) -> bool {
        self.routes.contains_key(plugin)
    }

    /// Every plugin that serves mounts, in id order.
    pub fn plugins(&self) -> impl Iterator<Item = &str> {
        self.routes.keys().map(String::as_str)
    }

    /// The provider for `plugin`'s mounts, and the context to call it with:
    /// one in the plugin's own name, so any notice it posts lands as its own.
    pub fn route(
        &self,
        plugin: &str,
        session_id: &str,
        notices: &Arc<Notices>,
    ) -> Option<(SharedProvider, SessionCtx)> {
        let route = self.routes.get(plugin)?;
        Some((
            Arc::clone(&route.provider),
            SessionCtx::new(session_id, plugin, route.order, Arc::clone(notices)),
        ))
    }
}

/// Normalizes a path inside a mount to the form a provider is handed:
/// absolute, `/`-separated, no `.`/`..`/empty segments, no trailing `/`
/// (except the root itself). `..` above the root is refused rather than
/// clamped, so a path can never name something outside its mount.
pub fn normalize_path(path: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    Some(format!("/{}", segments.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_normalize_and_never_climb_out() {
        assert_eq!(normalize_path("").as_deref(), Some("/"));
        assert_eq!(normalize_path("/").as_deref(), Some("/"));
        assert_eq!(normalize_path("a//b/./c/").as_deref(), Some("/a/b/c"));
        assert_eq!(normalize_path("/a/b/../c").as_deref(), Some("/a/c"));
        assert_eq!(normalize_path("/.."), None);
        assert_eq!(normalize_path("/a/../.."), None);
    }

    #[test]
    fn components_may_not_export_provider() {
        assert!(check_component_exports(&[MOUNTS.to_owned()]).is_ok());
        let refused = check_component_exports(&[PROVIDER.to_owned()]).unwrap_err();
        assert!(refused.contains("built-in"), "{refused}");
    }

    #[test]
    fn errors_serialize_as_the_wit_names_them() {
        assert_eq!(
            serde_json::to_value(MountError::ScratchDisabled).unwrap(),
            "scratch-disabled"
        );
        assert_eq!(
            serde_json::to_value(FsError::NotADirectory).unwrap(),
            "not-a-directory"
        );
    }

    #[test]
    fn the_wit_declares_both_interfaces() {
        assert!(WIT.contains("interface mounts"));
        assert!(WIT.contains("interface provider"));
        for op in [
            "stat:",
            "read-dir:",
            "read:",
            "put:",
            "mkdir:",
            "rename:",
            "remove:",
        ] {
            assert!(WIT.contains(op), "{op}");
        }
    }
}
