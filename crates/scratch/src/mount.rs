//! Mounts: other plugins' filesystems, live under `/mnt/<plugin>/<name>`.
//!
//! A mount is served by a [`MountBackend`] — the seven provider operations and
//! nothing else. Both modes reach the same backend through the same table:
//! virtual mode dispatches to it from the shell's filesystem, compute mode from
//! one FUSE connection that serves all of `/mnt`. Because the table is shared
//! and consulted per call, a mount added while the sandbox runs shows up
//! without remounting anything.
//!
//! FUSE can only hand a program an errno, so the real error is also written to
//! [`ERRORS_PATH`], a read-only file both modes serve. Virtual mode logs the
//! same way, so the file means the same thing whichever mode wrote it.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

/// Where every mount lives. Environments chooses it; no plugin can name a
/// path outside it.
pub const MNT: &str = "/mnt";

/// Where the real error behind a failed mount operation is logged, one line
/// per failure, newest last.
pub const ERRORS_PATH: &str = "/mnt/.errors";

/// The file name of [`ERRORS_PATH`] inside `/mnt`.
pub(crate) const ERRORS_NAME: &str = ".errors";

/// How many failures [`ERRORS_PATH`] keeps. Old lines are dropped first: the
/// file is for the failure that just happened, not an audit trail.
const ERRORS_KEPT: usize = 200;

/// How many mounts one session may hold.
pub const MAX_MOUNTS: usize = 64;

/// The provider interface's own types, so a backend written against
/// `plugin::environments::Provider` hands its results straight through.
pub use plugin::environments::{Entry, FsError, Kind};

/// The same error as an `io::Error`, worded the way coreutils word its errno,
/// so a virtual `cat` prints what a real one would. Lossy by nature — a
/// conflict and a stale handle are one errno — which is why the real error is
/// also logged to [`ERRORS_PATH`].
pub(crate) fn fs_error_io(error: FsError) -> std::io::Error {
    crate::vfs::errno_error(error.errno())
}

/// The seven provider operations, bound to one mount.
///
/// Every `path` is absolute within the mount: `/` is the mount's root, and
/// `/notes/a.md` is `/mnt/<plugin>/<name>/notes/a.md` in scratch. The session
/// context and the mount name the provider interface also takes are the
/// integrator's to bind — one backend instance is one mount in one session.
///
/// Writes replace whole files. Under exec the filesystem layer buffers a
/// file's edits and calls [`put`](MountBackend::put) once, on flush.
#[async_trait]
pub trait MountBackend: Send + Sync {
    async fn stat(&self, path: &str) -> Result<Entry, FsError>;

    async fn read_dir(&self, path: &str) -> Result<Vec<Entry>, FsError>;

    async fn read(&self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>, FsError>;

    /// Whole-file write: create, replace, or fail with
    /// [`FsError::Conflict`] when `if_version` no longer matches, or with
    /// [`FsError::Exists`] when `create_only` and the file exists.
    async fn put(
        &self,
        path: &str,
        data: Vec<u8>,
        if_version: Option<String>,
        create_only: bool,
    ) -> Result<Entry, FsError>;

    async fn mkdir(&self, path: &str) -> Result<Entry, FsError>;

    async fn rename(&self, src: &str, dst: &str) -> Result<(), FsError>;

    async fn remove(&self, path: &str, recursive: bool) -> Result<(), FsError>;
}

/// The largest mount file scratch holds whole in the Faber process: reading
/// one into a buffer, writing one through FUSE, or truncating one up. Past it
/// the operation fails with `too-large` (`EFBIG`) instead of letting one
/// sandbox grow the server's memory without bound.
pub const MAX_FILE: u64 = 256 << 20;

/// Reads a whole file from a backend: `stat`, then `read` at increasing
/// offsets until the reported size is reached or the provider returns less
/// than asked, whichever comes first — a provider whose size is a guess must
/// not make the read loop forever or stop short. A file past [`MAX_FILE`] is
/// refused.
pub(crate) async fn read_all(backend: &dyn MountBackend, path: &str) -> Result<Vec<u8>, FsError> {
    let entry = backend.stat(path).await?;
    if entry.kind == Kind::Dir {
        return Err(FsError::IsADirectory);
    }
    if entry.size > MAX_FILE {
        return Err(FsError::TooLarge);
    }
    const CHUNK: u32 = 1 << 20;
    let mut bytes = Vec::with_capacity(entry.size as usize);
    loop {
        let chunk = backend.read(path, bytes.len() as u64, CHUNK).await?;
        let short = chunk.len() < CHUNK as usize;
        if bytes.len() as u64 + chunk.len() as u64 > MAX_FILE {
            return Err(FsError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
        if chunk.is_empty() || short {
            return Ok(bytes);
        }
    }
}

/// Why a mount or unmount was refused: the provider interface's own error.
/// Scratch only ever returns `InvalidName`, `NotFound` and `TooMany`; the
/// integrator adds the two that are about the project rather than the session.
pub use plugin::environments::MountError;

/// A mount as the table holds it.
#[derive(Clone)]
pub struct Mount {
    pub read_only: bool,
    pub backend: Arc<dyn MountBackend>,
}

impl fmt::Debug for Mount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mount")
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

/// One session's mounts, by plugin and name.
#[derive(Default)]
pub struct MountTable {
    mounts: RwLock<BTreeMap<(String, String), Mount>>,
}

impl MountTable {
    /// Adds a mount, replacing one with the same plugin and name. Returns the
    /// path it is mounted at.
    pub fn mount(
        &self,
        plugin: &str,
        name: &str,
        read_only: bool,
        backend: Arc<dyn MountBackend>,
    ) -> Result<String, MountError> {
        if !valid_component(plugin) || !valid_component(name) {
            return Err(MountError::InvalidName);
        }
        let mut mounts = self.mounts.write().expect("mount table poisoned");
        let key = (plugin.to_owned(), name.to_owned());
        if !mounts.contains_key(&key) && mounts.len() >= MAX_MOUNTS {
            return Err(MountError::TooMany);
        }
        mounts.insert(key, Mount { read_only, backend });
        Ok(format!("{MNT}/{plugin}/{name}"))
    }

    pub fn unmount(&self, plugin: &str, name: &str) -> Result<(), MountError> {
        let mut mounts = self.mounts.write().expect("mount table poisoned");
        mounts
            .remove(&(plugin.to_owned(), name.to_owned()))
            .map(|_| ())
            .ok_or(MountError::NotFound)
    }

    pub fn get(&self, plugin: &str, name: &str) -> Option<Mount> {
        let mounts = self.mounts.read().expect("mount table poisoned");
        mounts.get(&(plugin.to_owned(), name.to_owned())).cloned()
    }

    /// Every `(plugin, name, read_only)`, sorted.
    pub fn list(&self) -> Vec<(String, String, bool)> {
        let mounts = self.mounts.read().expect("mount table poisoned");
        mounts
            .iter()
            .map(|((plugin, name), mount)| (plugin.clone(), name.clone(), mount.read_only))
            .collect()
    }

    pub(crate) fn plugins(&self) -> Vec<String> {
        let mut plugins: Vec<String> = self.list().into_iter().map(|(p, _, _)| p).collect();
        plugins.dedup();
        plugins
    }

    pub(crate) fn names(&self, plugin: &str) -> Vec<String> {
        self.list()
            .into_iter()
            .filter(|(p, _, _)| p == plugin)
            .map(|(_, name, _)| name)
            .collect()
    }
}

/// A plugin id or mount name usable as one path component: letters, digits,
/// `.`, `_`, `-`, not starting with `.` (so it can never be `.`, `..`, or the
/// errors file) and at most 128 bytes.
pub(crate) fn valid_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A path under `/mnt`, classified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MntPath {
    /// `/mnt` itself.
    Root,
    /// `/mnt/.errors`.
    Errors,
    /// `/mnt/<plugin>`.
    Plugin(String),
    /// `/mnt/<plugin>/<name>/...`, with the rest as a mount-absolute path.
    Inside {
        plugin: String,
        name: String,
        path: String,
    },
}

/// Classifies a normalized absolute path, or `None` when it is not under
/// `/mnt`.
pub(crate) fn classify(path: &str) -> Option<MntPath> {
    let rest = path.strip_prefix(MNT)?;
    if rest.is_empty() {
        return Some(MntPath::Root);
    }
    let rest = rest.strip_prefix('/')?;
    let mut parts = rest.splitn(3, '/');
    let plugin = parts.next().unwrap_or_default();
    if plugin == ERRORS_NAME && parts.clone().next().is_none() {
        return Some(MntPath::Errors);
    }
    let Some(name) = parts.next() else {
        return Some(MntPath::Plugin(plugin.to_owned()));
    };
    let inner = parts.next().unwrap_or_default();
    Some(MntPath::Inside {
        plugin: plugin.to_owned(),
        name: name.to_owned(),
        path: format!("/{inner}"),
    })
}

/// The log behind [`ERRORS_PATH`].
#[derive(Default)]
pub struct ErrorLog {
    lines: Mutex<VecDeque<String>>,
}

impl ErrorLog {
    /// Records a failure. `path` is the scratch path the program used, so the
    /// agent can match a line to the command that failed.
    pub fn record(&self, op: &str, path: &str, error: FsError) {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        // The provider interface's own name for the error first, so a line
        // reads the same as the error a plugin's documentation describes.
        let name = serde_json::to_value(error)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let line = format!("{at} {op} {path}: {name} ({error})\n");
        tracing::debug!(target: "scratch::mounts", "{}", line.trim_end());
        let mut lines = self.lines.lock().expect("error log poisoned");
        if lines.len() >= ERRORS_KEPT {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn render(&self) -> Vec<u8> {
        let lines = self.lines.lock().expect("error log poisoned");
        lines.iter().flat_map(|line| line.bytes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_under_mnt_are_classified() {
        assert_eq!(classify("/mnt"), Some(MntPath::Root));
        assert_eq!(classify("/mnt/.errors"), Some(MntPath::Errors));
        assert_eq!(classify("/mnt/docs"), Some(MntPath::Plugin("docs".into())));
        assert_eq!(
            classify("/mnt/docs/notes"),
            Some(MntPath::Inside {
                plugin: "docs".into(),
                name: "notes".into(),
                path: "/".into()
            })
        );
        assert_eq!(
            classify("/mnt/docs/notes/a/b.md"),
            Some(MntPath::Inside {
                plugin: "docs".into(),
                name: "notes".into(),
                path: "/a/b.md".into()
            })
        );
        assert_eq!(classify("/mntx"), None);
        assert_eq!(classify("/etc"), None);
    }

    #[test]
    fn names_that_could_escape_or_collide_are_refused() {
        assert!(valid_component("notes-1.v2"));
        for bad in ["", ".", "..", ".errors", "a/b", "a b", &"x".repeat(129)] {
            assert!(!valid_component(bad), "{bad:?} should be refused");
        }
    }

    #[test]
    fn the_error_log_keeps_the_newest_lines() {
        let log = ErrorLog::default();
        for i in 0..(ERRORS_KEPT + 5) {
            log.record("put", &format!("/mnt/p/n/{i}"), FsError::Conflict);
        }
        let text = String::from_utf8(log.render()).unwrap();
        assert_eq!(text.lines().count(), ERRORS_KEPT);
        assert!(text.ends_with(&format!(
            "/mnt/p/n/{}: conflict (changed since it was read)\n",
            ERRORS_KEPT + 4
        )));
        assert!(!text.contains("/mnt/p/n/0:"));
    }
}
