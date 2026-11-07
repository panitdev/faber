//! Scratch's `/` as virtual mode sees it: the read-only base image under the
//! session's layer, the volumes, the toolstore, and the mounts.
//!
//! This is the filesystem the virtual shell runs over, and the one `read` and
//! `patch` use while scratch is virtual. It answers the way the container's
//! root would, because compute runs the same image and binds the same
//! volumes:
//!
//! - Lookups go to the layer first, then to the base, unless the layer deleted
//!   the path (a whiteout) or replaced the directory above it wholesale (an
//!   opaque directory).
//! - Writes copy up into the layer. The base directory is never written.
//! - Symlinks are resolved here, component by component, inside scratch's own
//!   namespace. The base image is full of absolute symlinks (`/bin` is one on
//!   Debian); following them through the host would leave the image. A link
//!   inside a volume resolves the same way, so it may lead anywhere in
//!   scratch, as it does in the container.
//! - Every host operation reaches its file through directories opened without
//!   following symlinks ([`Beneath`]), and a path is resolved and used under
//!   one hold of the change lock. Two runs in the same session share the
//!   layer, so one could otherwise swap a directory for a symlink between the
//!   other resolving a path and the host following it out of the layer.
//! - A bind shows a [`Volume`] at a fixed path as itself, with no layer over
//!   it: the project's workspace and `/tmp`, writable and shared with
//!   compute, and the operator's toolstore at `/opt/toolstore`, read-only.
//!   Writes there go straight to the volume (in tenantd, through its API),
//!   renames out of one fail with `EXDEV` as they do across mounts, and the
//!   tenant's quota on the volume, not the session's counter, bounds them.
//! - `/mnt` belongs to the mounts, whatever the base has there.
//!
//! [`stat`](FileSystem::stat) follows a final symlink, as `stat(2)` does, so
//! `[ -d /bin ]` and `command -v` agree with the sandbox; the shell commands
//! that would need `lstat` are not run virtually.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bashkit::{DirEntry, FileSystem, FileSystemExt, FileType, Metadata};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::beneath::{self, Beneath};
use crate::layer::{Layer, ancestors, is_at_or_under};
use crate::mount::{
    self, ErrorLog, FsError, Kind, MNT, MntPath, Mount, MountTable, classify, read_all,
};
use crate::quota::Usage;
use crate::volume::{EntryType, Meta, Volume, read_names};

/// Where the operator's toolstore appears in scratch.
pub const TOOLSTORE: &str = "/opt/toolstore";

/// A volume shown at `at` as it is, over whatever the base has there.
#[derive(Clone)]
pub struct Bind {
    /// An absolute scratch path, never `/`, never under `/mnt` or another
    /// bind.
    pub at: String,
    pub volume: Arc<dyn Volume>,
    pub read_only: bool,
}

impl fmt::Debug for Bind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bind")
            .field("at", &self.at)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

/// How many symlinks one lookup may follow, as Linux allows.
const MAX_SYMLINKS: usize = 40;

/// An errno carried inside an `io::Error`, worded as coreutils word it.
#[derive(Debug)]
struct Errno(i32);

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self.0 {
            libc::ENOENT => "No such file or directory",
            libc::EEXIST => "File exists",
            libc::ENOTDIR => "Not a directory",
            libc::EISDIR => "Is a directory",
            libc::ENOTEMPTY => "Directory not empty",
            libc::EROFS => "Read-only file system",
            libc::EIO => "Input/output error",
            libc::EFBIG => "File too large",
            libc::EACCES => "Permission denied",
            libc::EPERM => "Operation not permitted",
            libc::EXDEV => "Invalid cross-device link",
            libc::ELOOP => "Too many levels of symbolic links",
            libc::EDQUOT => "Disk quota exceeded",
            libc::EBUSY => "Device or resource busy",
            libc::EINVAL => "Invalid argument",
            libc::ESTALE => "Stale file handle",
            libc::ENOSPC => "No space left on device",
            _ => "Input/output error",
        };
        f.write_str(text)
    }
}

impl std::error::Error for Errno {}

/// An `io::Error` for `code`, with the matching kind and coreutils' wording.
pub(crate) fn errno_error(code: i32) -> io::Error {
    use io::ErrorKind as K;
    let kind = match code {
        libc::ENOENT => K::NotFound,
        libc::EEXIST => K::AlreadyExists,
        libc::ENOTDIR => K::NotADirectory,
        libc::EISDIR => K::IsADirectory,
        libc::ENOTEMPTY => K::DirectoryNotEmpty,
        libc::EROFS => K::ReadOnlyFilesystem,
        libc::EFBIG => K::FileTooLarge,
        libc::EACCES | libc::EPERM => K::PermissionDenied,
        libc::EXDEV => K::CrossesDevices,
        libc::EDQUOT => K::QuotaExceeded,
        libc::EBUSY => K::ResourceBusy,
        libc::EINVAL => K::InvalidInput,
        _ => K::Other,
    };
    io::Error::new(kind, Errno(code))
}

fn fail<T>(code: i32) -> io::Result<T> {
    Err(errno_error(code))
}

/// What a resolved path names: a place under `/mnt`, or a path in the tree.
enum At {
    Mnt(MntPath),
    Tree(String),
}

/// Where an entry of the tree really is.
enum Real {
    /// In the layer or the base, on this host.
    Host(Beneath),
    /// In a bind's volume, at this path inside it.
    Volume(Arc<dyn Volume>, String),
}

/// An entry found in the tree, with where it really is.
struct Found {
    real: Real,
    in_layer: bool,
    meta: Meta,
}

impl Found {
    fn is_dir(&self) -> bool {
        self.meta.is_dir()
    }

    fn is_symlink(&self) -> bool {
        self.meta.is_symlink()
    }
}

/// Where a path is to be created or changed: in the layer's tree on this
/// host, or in a bind's volume.
enum Upper {
    Host(Beneath),
    Volume(Arc<dyn Volume>, String),
}

/// The change lock, held to read or to change the tree.
#[allow(dead_code)]
enum Hold<'a> {
    Read(RwLockReadGuard<'a, ()>),
    Write(RwLockWriteGuard<'a, ()>),
}

type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A path inside a bind as its volume takes it: `/` for the bind itself.
fn inside(rest: &str) -> String {
    if rest.is_empty() {
        "/".to_owned()
    } else {
        rest.to_owned()
    }
}

/// Scratch's merged filesystem for one session.
pub struct ScratchFs {
    base: PathBuf,
    binds: Vec<Bind>,
    layer: Arc<Layer>,
    mounts: Arc<MountTable>,
    errors: Arc<ErrorLog>,
    usage: Usage,
    /// The version a mount file had when this session last read it, so the
    /// next whole-file write of it is a conditional one and a concurrent edit
    /// surfaces as a conflict rather than being overwritten.
    versions: Mutex<HashMap<String, String>>,
    /// Held to read the tree, and held exclusively to change it, from
    /// resolving a path to the last operation on it: a copy-up and the marks
    /// that go with it are never interleaved with another change, and
    /// nothing changes between resolving a path and using it.
    changing: RwLock<()>,
}

impl ScratchFs {
    pub fn new(
        base: PathBuf,
        binds: Vec<Bind>,
        layer: Arc<Layer>,
        mounts: Arc<MountTable>,
        errors: Arc<ErrorLog>,
        usage: Usage,
    ) -> Self {
        ScratchFs {
            base,
            binds,
            layer,
            mounts,
            errors,
            usage,
            versions: Mutex::default(),
            changing: RwLock::default(),
        }
    }

    pub fn layer(&self) -> &Arc<Layer> {
        &self.layer
    }

    pub fn mounts(&self) -> &Arc<MountTable> {
        &self.mounts
    }

    pub fn errors(&self) -> &Arc<ErrorLog> {
        &self.errors
    }

    /// The bind `path` is in and the path inside it.
    fn bind_at<'a>(&self, path: &'a str) -> Option<(&Bind, &'a str)> {
        self.binds
            .iter()
            .find(|bind| is_at_or_under(path, &bind.at))
            .map(|bind| (bind, &path[bind.at.len()..]))
    }

    fn bound(&self, path: &str) -> bool {
        self.bind_at(path).is_some()
    }

    /// In a bind nothing may change.
    fn read_only(&self, path: &str) -> bool {
        self.bind_at(path).is_some_and(|(bind, _)| bind.read_only)
    }

    /// Where `path` is to be created or changed: in its bind, or in the
    /// layer's tree.
    fn upper(&self, path: &str) -> io::Result<Upper> {
        match self.bind_at(path) {
            Some((bind, rest)) => Ok(Upper::Volume(Arc::clone(&bind.volume), inside(rest))),
            None => Beneath::new(self.layer.upper(), path).map(Upper::Host),
        }
    }

    /// What is at `path` in the tree, without following a final symlink.
    async fn lookup(&self, path: &str) -> io::Result<Option<Found>> {
        if let Some((bind, rest)) = self.bind_at(path) {
            let rest = inside(rest);
            // A writable bind is changed in place, as the layer is.
            return Ok(bind.volume.lstat(&rest).await?.map(|meta| Found {
                real: Real::Volume(Arc::clone(&bind.volume), rest),
                in_layer: !bind.read_only,
                meta,
            }));
        }
        if self.layer.is_whiteout(path) {
            return Ok(None);
        }
        if let Some(found) = found(self.layer.upper(), path, true)? {
            return Ok(Some(found));
        }
        if self.layer.under_opaque(path) {
            return Ok(None);
        }
        found(&self.base, path, false)
    }

    /// True when the base contributes something visible at `path`.
    fn base_has(&self, path: &str) -> io::Result<bool> {
        if self.layer.under_opaque(path) || self.bound(path) {
            return Ok(false);
        }
        Ok(found(&self.base, path, false)?.is_some())
    }

    async fn read_link_of(&self, found: &Found) -> io::Result<PathBuf> {
        match &found.real {
            Real::Host(real) => fs::read_link(real),
            Real::Volume(volume, path) => volume.read_link(path).await,
        }
    }

    async fn read_bytes(&self, found: &Found) -> io::Result<Vec<u8>> {
        match &found.real {
            Real::Host(real) => {
                let mut bytes = Vec::new();
                io::Read::read_to_end(&mut beneath::open_nofollow(real)?, &mut bytes)?;
                Ok(bytes)
            }
            Real::Volume(volume, path) => volume.read(path).await,
        }
    }

    /// Resolves `path` as [`resolve`](Self::resolve) does, holding the tree
    /// still while it does.
    pub(crate) async fn resolve_now(
        &self,
        path: &str,
        follow: bool,
        missing: bool,
    ) -> io::Result<String> {
        let _hold = self.hold(false).await;
        self.resolve(path, follow, missing).await
    }

    async fn hold(&self, write: bool) -> Hold<'_> {
        if write {
            Hold::Write(self.changing.write().await)
        } else {
            Hold::Read(self.changing.read().await)
        }
    }

    /// Resolves `path` (absolute) to its canonical scratch path, following
    /// symlinks in every component but the last, and the last too when
    /// `follow` is set. A missing last component is allowed when `missing` is
    /// set, which is how a create finds where it would land.
    pub(crate) async fn resolve(
        &self,
        path: &str,
        follow: bool,
        missing: bool,
    ) -> io::Result<String> {
        let mut pending: VecDeque<String> = components(path).collect();
        let mut done: Vec<String> = Vec::new();
        let mut hops = 0;
        while let Some(part) = pending.pop_front() {
            match part.as_str() {
                "." => continue,
                ".." => {
                    done.pop();
                    continue;
                }
                _ => {}
            }
            let candidate = join(&done, &part);
            let last = pending.is_empty();
            if classify(&candidate).is_some() {
                // Mounts have no symlinks: everything under /mnt is the
                // provider's and resolves lexically.
                done.push(part);
                continue;
            }
            match self.lookup(&candidate).await? {
                None if last && missing => done.push(part),
                None => return fail(libc::ENOENT),
                Some(found) if found.is_symlink() && (!last || follow) => {
                    hops += 1;
                    if hops > MAX_SYMLINKS {
                        return fail(libc::ELOOP);
                    }
                    let target = self.read_link_of(&found).await?;
                    let target = target.to_string_lossy().into_owned();
                    if target.starts_with('/') {
                        done.clear();
                    }
                    for piece in components(&target).collect::<Vec<_>>().into_iter().rev() {
                        pending.push_front(piece);
                    }
                }
                Some(found) => {
                    if !last && !found.is_dir() {
                        return fail(libc::ENOTDIR);
                    }
                    done.push(part);
                }
            }
        }
        Ok(join_all(&done))
    }

    /// Resolves `path` with the change lock already held: a place under
    /// `/mnt`, or the resolved path in the tree.
    async fn place(&self, path: &Path, follow: bool, missing: bool) -> io::Result<At> {
        let path = path.to_str().ok_or_else(|| errno_error(libc::EINVAL))?;
        if !path.starts_with('/') {
            return fail(libc::EINVAL);
        }
        let resolved = self.resolve(path, follow, missing).await?;
        Ok(match classify(&resolved) {
            Some(mnt) => At::Mnt(mnt),
            None => At::Tree(resolved),
        })
    }

    fn mount(&self, plugin: &str, name: &str) -> io::Result<Mount> {
        self.mounts
            .get(plugin, name)
            .ok_or_else(|| errno_error(libc::ENOENT))
    }

    /// Runs a provider operation, logging its real error where the agent can
    /// read it before turning it into an errno.
    async fn provider<T>(
        &self,
        op: &str,
        plugin: &str,
        name: &str,
        path: &str,
        call: impl std::future::Future<Output = Result<T, FsError>>,
    ) -> io::Result<T> {
        call.await.map_err(|error| {
            if error != FsError::NotFound {
                self.errors
                    .record(op, &scratch_path(plugin, name, path), error);
            }
            mount::fs_error_io(error)
        })
    }

    /// Reads a whole mount file and remembers its version for the next write.
    pub(crate) async fn mount_read(
        &self,
        plugin: &str,
        name: &str,
        path: &str,
    ) -> io::Result<Vec<u8>> {
        let mount = self.mount(plugin, name)?;
        let backend = Arc::clone(&mount.backend);
        let entry = self
            .provider("stat", plugin, name, path, backend.stat(path))
            .await?;
        let bytes = self
            .provider("read", plugin, name, path, read_all(backend.as_ref(), path))
            .await?;
        self.versions
            .lock()
            .expect("versions poisoned")
            .insert(scratch_path(plugin, name, path), entry.version);
        Ok(bytes)
    }

    /// Writes a whole mount file, conditional on the version last read.
    pub(crate) async fn mount_write(
        &self,
        plugin: &str,
        name: &str,
        path: &str,
        data: Vec<u8>,
    ) -> io::Result<mount::Entry> {
        let mount = self.mount(plugin, name)?;
        let key = scratch_path(plugin, name, path);
        if mount.read_only {
            self.errors.record("put", &key, FsError::ReadOnly);
            return fail(libc::EROFS);
        }
        let seen = self
            .versions
            .lock()
            .expect("versions poisoned")
            .get(&key)
            .cloned();
        let entry = self
            .provider(
                "put",
                plugin,
                name,
                path,
                mount.backend.put(path, data, seen, false),
            )
            .await?;
        self.versions
            .lock()
            .expect("versions poisoned")
            .insert(key, entry.version.clone());
        Ok(entry)
    }

    fn mnt_metadata(&self, mnt: &MntPath) -> io::Result<Metadata> {
        match mnt {
            MntPath::Root => Ok(dir_metadata()),
            MntPath::Errors => Ok(Metadata {
                file_type: FileType::File,
                size: self.errors.render().len() as u64,
                mode: 0o444,
                modified: SystemTime::now(),
                created: SystemTime::now(),
            }),
            MntPath::Plugin(plugin) => {
                if self.mounts.plugins().iter().any(|p| p == plugin) {
                    Ok(dir_metadata())
                } else {
                    fail(libc::ENOENT)
                }
            }
            MntPath::Inside { .. } => fail(libc::EINVAL),
        }
    }

    /// The merged listing of a directory in the tree, by name.
    async fn list_tree(&self, path: &str) -> io::Result<BTreeMap<String, Meta>> {
        if let Some((bind, rest)) = self.bind_at(path) {
            return Ok(bind
                .volume
                .read_dir(&inside(rest))
                .await?
                .into_iter()
                .collect());
        }
        let mut names: BTreeMap<String, Meta> =
            read_names(self.layer.upper(), path)?.into_iter().collect();
        if !self.layer.is_opaque(path) && !self.layer.under_opaque(path) {
            for (name, meta) in read_names(&self.base, path)? {
                let child = child_of(path, &name);
                if !names.contains_key(&name) && !self.layer.is_whiteout(&child) {
                    names.insert(name, meta);
                }
            }
        }
        if path == "/" {
            names.remove("mnt");
            if let Ok(meta) = fs::symlink_metadata(&self.base) {
                names.insert("mnt".to_owned(), Meta::from_std(&meta));
            }
        }
        for bind in &self.binds {
            if parent_of(&bind.at) == path
                && let Ok(Some(meta)) = bind.volume.lstat("/").await
            {
                names.insert(
                    bind.at[bind.at.rfind('/').unwrap_or(0) + 1..].to_owned(),
                    meta,
                );
            }
        }
        Ok(names)
    }

    /// Makes sure the directory at `path` exists where it is changed,
    /// creating it and its ancestors with the modes they have in the merged
    /// view.
    async fn ensure_dir(&self, path: &str) -> io::Result<()> {
        let mut chain: Vec<&str> = ancestors(path).collect();
        chain.reverse();
        chain.push(path);
        for dir in chain {
            if dir == "/" {
                continue;
            }
            match self.upper(dir)? {
                Upper::Volume(volume, inner) => {
                    if volume.lstat(&inner).await?.is_none() {
                        volume.mkdir(&inner, None, false).await?;
                    }
                }
                Upper::Host(real) => {
                    if lstat(&real)?.is_some() {
                        continue;
                    }
                    let mode = match self.lookup(dir).await? {
                        Some(found) if found.is_dir() => found.meta.mode,
                        Some(_) => return fail(libc::ENOTDIR),
                        None => 0o755,
                    };
                    fs::create_dir(&real)?;
                    beneath::set_mode(&real, mode | 0o700)?;
                }
            }
        }
        Ok(())
    }

    /// Brings a base entry into the layer so it can be changed in place.
    async fn copy_up(&self, path: &str, found: &Found) -> io::Result<()> {
        if found.in_layer {
            return Ok(());
        }
        if self.bound(path) {
            return fail(libc::EROFS);
        }
        if found.is_dir() {
            return self.ensure_dir(path).await;
        }
        self.ensure_dir(parent_of(path)).await?;
        let Upper::Host(real) = self.upper(path)? else {
            return fail(libc::EROFS);
        };
        if found.is_symlink() {
            symlink(self.read_link_of(found).await?, &real)?;
            return Ok(());
        }
        self.usage
            .change(0, found.meta.size)
            .map_err(|_| errno_error(libc::EDQUOT))?;
        self.copy_file_into(found, &real).await?;
        Ok(())
    }

    /// Copies a regular file of the tree to `to` in the layer, with its mode.
    async fn copy_file_into(&self, found: &Found, to: &Path) -> io::Result<()> {
        match &found.real {
            Real::Host(real) => beneath::copy_file(real, to).map(|_| ()),
            Real::Volume(volume, path) => {
                let bytes = volume.read(path).await?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(found.meta.mode)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(to)?;
                file.write_all(&bytes)?;
                file.set_permissions(fs::Permissions::from_mode(found.meta.mode))
            }
        }
    }

    /// Writes a regular file in the tree, copying it up first.
    async fn write_tree(&self, path: &str, content: &[u8], append: bool) -> io::Result<()> {
        if self.read_only(path) {
            return fail(libc::EROFS);
        }
        let existing = self.lookup(path).await?;
        let mode = match &existing {
            Some(found) if found.is_dir() => return fail(libc::EISDIR),
            Some(found) => {
                if append {
                    self.copy_up(path, found).await?;
                } else {
                    self.ensure_dir(parent_of(path)).await?;
                }
                found.meta.mode
            }
            None => {
                self.ensure_dir(parent_of(path)).await?;
                0o644
            }
        };
        let real = match self.upper(path)? {
            Upper::Volume(volume, inner) => {
                return if append {
                    volume.append(&inner, content).await
                } else {
                    volume.write(&inner, content, Some(mode)).await
                };
            }
            Upper::Host(real) => real,
        };
        let before = lstat(&real)?.map_or(0, |meta| meta.len());
        let after = if append {
            before + content.len() as u64
        } else {
            content.len() as u64
        };
        self.usage
            .change(before, after)
            .map_err(|_| errno_error(libc::EDQUOT))?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(append)
            .truncate(!append)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&real)?;
        file.write_all(content)?;
        if !append {
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        self.layer.clear_whiteout(path)
    }

    async fn mkdir_tree(&self, path: &str) -> io::Result<()> {
        if self.read_only(path) {
            return fail(libc::EROFS);
        }
        if self.lookup(path).await?.is_some() {
            return fail(libc::EEXIST);
        }
        self.ensure_dir(parent_of(path)).await?;
        match self.upper(path)? {
            Upper::Volume(volume, inner) => return volume.mkdir(&inner, None, false).await,
            Upper::Host(real) => fs::create_dir(real)?,
        }
        if self.layer.is_whiteout(path) || self.base_has(path)? {
            // A directory made where the base had one that was deleted must
            // not bring the base's contents back.
            self.layer.clear_whiteout(path)?;
            self.layer.set_opaque(path)?;
        }
        Ok(())
    }

    /// Removes a tree entry and everything under it: drops the layer's copy
    /// and whites out the base's.
    async fn remove_tree(&self, path: &str) -> io::Result<()> {
        if self.read_only(path) {
            return fail(libc::EROFS);
        }
        if path == "/" || self.binds.iter().any(|bind| bind.at == path) {
            return fail(libc::EBUSY);
        }
        if let Some((bind, rest)) = self.bind_at(path) {
            let inner = inside(rest);
            return match bind.volume.lstat(&inner).await? {
                Some(meta) if meta.is_dir() => bind.volume.remove_all(&inner).await,
                Some(_) => bind.volume.remove(&inner, false).await,
                None => fail(libc::ENOENT),
            };
        }
        let in_base = self.base_has(path)?;
        if let Some((real, meta)) = host_entry(self.layer.upper(), path)? {
            let bytes = if meta.is_dir() {
                tree_bytes(&real)
            } else {
                meta.len()
            };
            if meta.is_dir() {
                fs::remove_dir_all(&real)?;
            } else {
                fs::remove_file(&real)?;
            }
            let _ = self.usage.change(bytes, 0);
        }
        self.layer.forget(path)?;
        if in_base {
            self.ensure_dir(parent_of(path)).await?;
            self.layer.add_whiteout(path)?;
        }
        Ok(())
    }

    /// Copies the merged view of `from` into the layer at `to`, where
    /// nothing is. A copy that fails part way — the quota, above all — is
    /// removed again, so a failed `mv` leaves no half of it behind.
    async fn copy_tree(&self, from: &str, to: &str) -> io::Result<()> {
        let copied = self.copy_entries(from, to).await;
        if copied.is_err()
            && let Ok(Some((real, meta))) = host_entry(self.layer.upper(), to)
        {
            let bytes = if meta.is_dir() {
                tree_bytes(&real)
            } else {
                meta.len()
            };
            let removed = if meta.is_dir() {
                fs::remove_dir_all(&real)
            } else {
                fs::remove_file(&real)
            };
            if removed.is_ok() {
                let _ = self.usage.change(bytes, 0);
            }
        }
        copied
    }

    fn copy_entries<'a>(&'a self, from: &'a str, to: &'a str) -> Boxed<'a, io::Result<()>> {
        Box::pin(async move {
            let Some(found) = self.lookup(from).await? else {
                return fail(libc::ENOENT);
            };
            let Upper::Host(target) = self.upper(to)? else {
                return fail(libc::EXDEV);
            };
            if found.is_symlink() {
                symlink(self.read_link_of(&found).await?, &target)?;
            } else if found.is_dir() {
                fs::create_dir(&target)?;
                beneath::set_mode(&target, found.meta.mode | 0o700)?;
                for name in self.list_tree(from).await?.keys() {
                    self.copy_entries(&child_of(from, name), &child_of(to, name))
                        .await?;
                }
            } else {
                self.usage
                    .change(0, found.meta.size)
                    .map_err(|_| errno_error(libc::EDQUOT))?;
                self.copy_file_into(&found, &target).await?;
            }
            Ok(())
        })
    }

    async fn rename_tree(&self, from: &str, to: &str) -> io::Result<()> {
        if self.read_only(from) || self.read_only(to) {
            return fail(libc::EROFS);
        }
        if from == to {
            return Ok(());
        }
        if is_at_or_under(to, from) {
            return fail(libc::EINVAL);
        }
        if self
            .binds
            .iter()
            .any(|bind| bind.at == from || bind.at == to)
        {
            return fail(libc::EBUSY);
        }
        match (self.bind_at(from), self.bind_at(to)) {
            (None, None) => {}
            (Some((a, from_rest)), Some((b, to_rest))) if a.at == b.at => {
                // One volume: its own rename, with its rules for what may
                // replace what.
                return a.volume.rename(&inside(from_rest), &inside(to_rest)).await;
            }
            _ => return fail(libc::EXDEV),
        }
        let Some(source) = self.lookup(from).await? else {
            return fail(libc::ENOENT);
        };
        match self.lookup(to).await? {
            Some(dest) if dest.is_dir() && !source.is_dir() => return fail(libc::EISDIR),
            Some(dest) if !dest.is_dir() && source.is_dir() => return fail(libc::ENOTDIR),
            Some(dest) if dest.is_dir() && !self.list_tree(to).await?.is_empty() => {
                return fail(libc::ENOTEMPTY);
            }
            Some(_) => self.remove_tree(to).await?,
            None => {}
        }
        self.ensure_dir(parent_of(to)).await?;
        let dest_base = self.base_has(to)? || self.layer.is_whiteout(to);
        let merged = source.is_dir() && self.base_has(from)?;
        match (&source.real, source.in_layer && !merged) {
            (Real::Host(real), true) => {
                let Upper::Host(target) = self.upper(to)? else {
                    return fail(libc::EXDEV);
                };
                fs::rename(real, target)?;
                self.layer.rename_marks(from, to)?;
            }
            _ => self.copy_tree(from, to).await?,
        }
        self.layer.clear_whiteout(to)?;
        if source.is_dir() && dest_base {
            self.layer.set_opaque(to)?;
        }
        if host_entry(self.layer.upper(), from)?.is_some() || self.base_has(from)? {
            self.remove_tree(from).await?;
        }
        Ok(())
    }

    /// The bytes of a resolved tree path, following the final symlink. For
    /// `read` and `patch`, which address files, not links.
    pub(crate) async fn read_tree(&self, path: &str) -> io::Result<Vec<u8>> {
        match self.lookup(path).await? {
            None => fail(libc::ENOENT),
            Some(found) if found.is_dir() => fail(libc::EISDIR),
            Some(found) => self.read_bytes(&found).await,
        }
    }
}

#[async_trait]
impl FileSystem for ScratchFs {
    async fn read_file(&self, path: &Path) -> bashkit::Result<Vec<u8>> {
        let hold = self.hold(false).await;
        Ok(match self.place(path, true, false).await? {
            At::Tree(path) => self.read_tree(&path).await?,
            At::Mnt(mnt) => {
                drop(hold);
                match mnt {
                    MntPath::Errors => self.errors.render(),
                    MntPath::Inside { plugin, name, path } => {
                        self.mount_read(&plugin, &name, &path).await?
                    }
                    _ => return Err(errno_error(libc::EISDIR).into()),
                }
            }
        })
    }

    async fn write_file(&self, path: &Path, content: &[u8]) -> bashkit::Result<()> {
        let hold = self.hold(true).await;
        match self.place(path, true, true).await? {
            At::Tree(path) => Ok(self.write_tree(&path, content, false).await?),
            At::Mnt(mnt) => {
                drop(hold);
                match mnt {
                    MntPath::Inside { plugin, name, path } => {
                        if path == "/" {
                            return Err(errno_error(libc::EISDIR).into());
                        }
                        self.mount_write(&plugin, &name, &path, content.to_vec())
                            .await?;
                        Ok(())
                    }
                    MntPath::Errors => Err(errno_error(libc::EACCES).into()),
                    _ => Err(errno_error(libc::EISDIR).into()),
                }
            }
        }
    }

    async fn append_file(&self, path: &Path, content: &[u8]) -> bashkit::Result<()> {
        let hold = self.hold(true).await;
        match self.place(path, true, true).await? {
            At::Tree(path) => Ok(self.write_tree(&path, content, true).await?),
            At::Mnt(mnt) => {
                drop(hold);
                match mnt {
                    MntPath::Inside { plugin, name, path } => {
                        let mut bytes = match self.mount_read(&plugin, &name, &path).await {
                            Ok(bytes) => bytes,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
                            Err(error) => return Err(error.into()),
                        };
                        bytes.extend_from_slice(content);
                        self.mount_write(&plugin, &name, &path, bytes).await?;
                        Ok(())
                    }
                    MntPath::Errors => Err(errno_error(libc::EACCES).into()),
                    _ => Err(errno_error(libc::EISDIR).into()),
                }
            }
        }
    }

    async fn mkdir(&self, path: &Path, recursive: bool) -> bashkit::Result<()> {
        if recursive {
            let text = path.to_str().ok_or_else(|| errno_error(libc::EINVAL))?;
            let mut prefix = String::new();
            for part in components(text) {
                match part.as_str() {
                    "." => continue,
                    ".." => {
                        prefix = parent_of(&prefix).trim_end_matches('/').to_owned();
                        continue;
                    }
                    _ => {}
                }
                prefix = format!("{prefix}/{part}");
                match self.stat(Path::new(&prefix)).await {
                    Ok(meta) if meta.file_type.is_dir() => continue,
                    Ok(_) => return Err(errno_error(libc::ENOTDIR).into()),
                    Err(_) => self.mkdir(Path::new(&prefix), false).await?,
                }
            }
            return Ok(());
        }
        let hold = self.hold(true).await;
        match self.place(path, false, true).await? {
            At::Tree(path) => Ok(self.mkdir_tree(&path).await?),
            At::Mnt(mnt) => {
                drop(hold);
                let MntPath::Inside { plugin, name, path } = mnt else {
                    return Err(errno_error(libc::EEXIST).into());
                };
                if path == "/" {
                    return Err(errno_error(libc::EEXIST).into());
                }
                let mount = self.mount(&plugin, &name)?;
                if mount.read_only {
                    self.errors.record(
                        "mkdir",
                        &scratch_path(&plugin, &name, &path),
                        FsError::ReadOnly,
                    );
                    return Err(errno_error(libc::EROFS).into());
                }
                self.provider("mkdir", &plugin, &name, &path, mount.backend.mkdir(&path))
                    .await?;
                Ok(())
            }
        }
    }

    async fn remove(&self, path: &Path, recursive: bool) -> bashkit::Result<()> {
        let hold = self.hold(true).await;
        match self.place(path, false, false).await? {
            At::Tree(path) => {
                let Some(found) = self.lookup(&path).await? else {
                    return Err(errno_error(libc::ENOENT).into());
                };
                if found.is_dir() && !recursive && !self.list_tree(&path).await?.is_empty() {
                    return Err(errno_error(libc::ENOTEMPTY).into());
                }
                Ok(self.remove_tree(&path).await?)
            }
            At::Mnt(mnt) => {
                drop(hold);
                let MntPath::Inside { plugin, name, path } = mnt else {
                    return Err(errno_error(libc::EBUSY).into());
                };
                if path == "/" {
                    return Err(errno_error(libc::EBUSY).into());
                }
                let mount = self.mount(&plugin, &name)?;
                if mount.read_only {
                    self.errors.record(
                        "remove",
                        &scratch_path(&plugin, &name, &path),
                        FsError::ReadOnly,
                    );
                    return Err(errno_error(libc::EROFS).into());
                }
                self.provider(
                    "remove",
                    &plugin,
                    &name,
                    &path,
                    mount.backend.remove(&path, recursive),
                )
                .await?;
                Ok(())
            }
        }
    }

    async fn stat(&self, path: &Path) -> bashkit::Result<Metadata> {
        let hold = self.hold(false).await;
        Ok(match self.place(path, true, false).await? {
            At::Tree(path) => match self.lookup(&path).await? {
                Some(found) => std_metadata(&found.meta),
                None => return Err(errno_error(libc::ENOENT).into()),
            },
            At::Mnt(MntPath::Inside { plugin, name, path }) => {
                drop(hold);
                let mount = self.mount(&plugin, &name)?;
                let entry = self
                    .provider("stat", &plugin, &name, &path, mount.backend.stat(&path))
                    .await?;
                entry_metadata(&entry)
            }
            At::Mnt(mnt) => self.mnt_metadata(&mnt)?,
        })
    }

    async fn read_dir(&self, path: &Path) -> bashkit::Result<Vec<DirEntry>> {
        let hold = self.hold(false).await;
        Ok(match self.place(path, true, false).await? {
            At::Tree(path) => {
                match self.lookup(&path).await? {
                    Some(found) if found.is_dir() => {}
                    Some(_) => return Err(errno_error(libc::ENOTDIR).into()),
                    None => return Err(errno_error(libc::ENOENT).into()),
                }
                self.list_tree(&path)
                    .await?
                    .into_iter()
                    .map(|(name, meta)| DirEntry {
                        name,
                        metadata: std_metadata(&meta),
                    })
                    .collect()
            }
            At::Mnt(MntPath::Root) => {
                let mut entries: Vec<DirEntry> = self
                    .mounts
                    .plugins()
                    .into_iter()
                    .map(|name| DirEntry {
                        name,
                        metadata: dir_metadata(),
                    })
                    .collect();
                entries.push(DirEntry {
                    name: mount::ERRORS_NAME.to_owned(),
                    metadata: self.mnt_metadata(&MntPath::Errors)?,
                });
                entries
            }
            At::Mnt(MntPath::Plugin(plugin)) => {
                let names = self.mounts.names(&plugin);
                if names.is_empty() {
                    return Err(errno_error(libc::ENOENT).into());
                }
                names
                    .into_iter()
                    .map(|name| DirEntry {
                        name,
                        metadata: dir_metadata(),
                    })
                    .collect()
            }
            At::Mnt(MntPath::Inside { plugin, name, path }) => {
                drop(hold);
                let mount = self.mount(&plugin, &name)?;
                self.provider(
                    "read-dir",
                    &plugin,
                    &name,
                    &path,
                    mount.backend.read_dir(&path),
                )
                .await?
                .iter()
                .map(|entry| DirEntry {
                    name: entry.name.clone(),
                    metadata: entry_metadata(entry),
                })
                .collect()
            }
            At::Mnt(MntPath::Errors) => return Err(errno_error(libc::ENOTDIR).into()),
        })
    }

    async fn exists(&self, path: &Path) -> bashkit::Result<bool> {
        Ok(self.stat(path).await.is_ok())
    }

    async fn rename(&self, from: &Path, to: &Path) -> bashkit::Result<()> {
        // Both ends are resolved and the move made under one hold of the
        // change lock.
        let hold = self.hold(true).await;
        let source = self.place(from, false, false).await?;
        let dest = self.place(to, false, true).await?;
        let (source, dest) = match (source, dest) {
            (At::Tree(from), At::Tree(to)) => {
                self.rename_tree(&from, &to).await?;
                return Ok(());
            }
            (At::Mnt(source), At::Mnt(dest)) => (source, dest),
            _ => return Err(errno_error(libc::EXDEV).into()),
        };
        drop(hold);
        match (source, dest) {
            (
                MntPath::Inside {
                    plugin,
                    name,
                    path: src,
                },
                MntPath::Inside {
                    plugin: to_plugin,
                    name: to_name,
                    path: dst,
                },
            ) if plugin == to_plugin && name == to_name => {
                let mount = self.mount(&plugin, &name)?;
                if mount.read_only {
                    self.errors.record(
                        "rename",
                        &scratch_path(&plugin, &name, &src),
                        FsError::ReadOnly,
                    );
                    return Err(errno_error(libc::EROFS).into());
                }
                self.provider(
                    "rename",
                    &plugin,
                    &name,
                    &src,
                    mount.backend.rename(&src, &dst),
                )
                .await?;
                Ok(())
            }
            _ => Err(errno_error(libc::EXDEV).into()),
        }
    }

    async fn copy(&self, from: &Path, to: &Path) -> bashkit::Result<()> {
        let bytes = self.read_file(from).await?;
        self.write_file(to, &bytes).await
    }

    async fn symlink(&self, target: &Path, link: &Path) -> bashkit::Result<()> {
        let _hold = self.hold(true).await;
        let At::Tree(path) = self.place(link, false, true).await? else {
            return Err(errno_error(libc::EPERM).into());
        };
        if self.read_only(&path) {
            return Err(errno_error(libc::EROFS).into());
        }
        if self.lookup(&path).await?.is_some() {
            return Err(errno_error(libc::EEXIST).into());
        }
        self.ensure_dir(parent_of(&path)).await?;
        match self.upper(&path)? {
            Upper::Volume(volume, inner) => {
                let target = target.to_str().ok_or_else(|| errno_error(libc::EINVAL))?;
                volume.symlink(target, &inner).await?;
            }
            Upper::Host(real) => {
                symlink(target, real)?;
                self.layer.clear_whiteout(&path)?;
            }
        }
        Ok(())
    }

    async fn read_link(&self, path: &Path) -> bashkit::Result<PathBuf> {
        let _hold = self.hold(false).await;
        let At::Tree(path) = self.place(path, false, false).await? else {
            return Err(errno_error(libc::EINVAL).into());
        };
        match self.lookup(&path).await? {
            Some(found) if found.is_symlink() => Ok(self.read_link_of(&found).await?),
            Some(_) => Err(errno_error(libc::EINVAL).into()),
            None => Err(errno_error(libc::ENOENT).into()),
        }
    }

    async fn chmod(&self, path: &Path, mode: u32) -> bashkit::Result<()> {
        // Mounts have no modes; accepting the change keeps `chmod` in a
        // script from failing on them, as it does under FUSE.
        self.change_attr(path, Some(mode & 0o7777), None).await?;
        Ok(())
    }

    async fn set_modified_time(&self, path: &Path, time: SystemTime) -> bashkit::Result<()> {
        self.change_attr(path, None, Some(time)).await?;
        Ok(())
    }
}

impl ScratchFs {
    /// `chmod` and `touch` on the tree, after a copy-up; nothing under
    /// `/mnt`.
    async fn change_attr(
        &self,
        path: &Path,
        mode: Option<u32>,
        time: Option<SystemTime>,
    ) -> io::Result<()> {
        let _hold = self.hold(true).await;
        let At::Tree(path) = self.place(path, true, false).await? else {
            return Ok(());
        };
        let Some(found) = self.lookup(&path).await? else {
            return fail(libc::ENOENT);
        };
        self.copy_up(&path, &found).await?;
        match self.upper(&path)? {
            Upper::Volume(volume, inner) => volume.set_attr(&inner, mode, time).await,
            Upper::Host(real) => {
                if let Some(mode) = mode {
                    beneath::set_mode(&real, mode)?;
                }
                if let Some(time) = time {
                    beneath::open_nofollow(&real)?.set_modified(time)?;
                }
                Ok(())
            }
        }
    }
}

#[async_trait]
impl FileSystemExt for ScratchFs {
    fn backend_kind(&self) -> &'static str {
        "faber-scratch"
    }
}

fn lstat(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if gone(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// What is at `relative` under `root` on this host and where, without
/// following a symlink anywhere on the way. A directory on the way that is
/// missing, or is not a directory, means nothing is there.
fn host_entry(root: &Path, relative: &str) -> io::Result<Option<(Beneath, fs::Metadata)>> {
    let real = match Beneath::new(root, relative) {
        Ok(real) => real,
        Err(error) if gone(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(lstat(&real)?.map(|meta| (real, meta)))
}

fn found(root: &Path, relative: &str, in_layer: bool) -> io::Result<Option<Found>> {
    Ok(host_entry(root, relative)?.map(|(real, meta)| Found {
        real: Real::Host(real),
        in_layer,
        meta: Meta::from_std(&meta),
    }))
}

fn gone(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn tree_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match fs::symlink_metadata(entry.path()) {
            Ok(meta) if meta.is_dir() => tree_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

pub(crate) fn components(path: &str) -> impl Iterator<Item = String> + '_ {
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
}

fn join(done: &[String], part: &str) -> String {
    if done.is_empty() {
        format!("/{part}")
    } else {
        format!("/{}/{part}", done.join("/"))
    }
}

fn join_all(done: &[String]) -> String {
    format!("/{}", done.join("/"))
}

pub(crate) fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(at) => &path[..at],
    }
}

pub(crate) fn child_of(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn scratch_path(plugin: &str, name: &str, path: &str) -> String {
    let rest = path.trim_start_matches('/');
    if rest.is_empty() {
        format!("{MNT}/{plugin}/{name}")
    } else {
        format!("{MNT}/{plugin}/{name}/{rest}")
    }
}

fn dir_metadata() -> Metadata {
    Metadata {
        file_type: FileType::Directory,
        size: 0,
        mode: 0o755,
        modified: SystemTime::now(),
        created: SystemTime::now(),
    }
}

pub(crate) fn entry_metadata(entry: &mount::Entry) -> Metadata {
    let modified = UNIX_EPOCH + Duration::from_secs(entry.mtime);
    match entry.kind {
        Kind::Dir => Metadata {
            file_type: FileType::Directory,
            size: 0,
            mode: 0o755,
            modified,
            created: modified,
        },
        Kind::File => Metadata {
            file_type: FileType::File,
            size: entry.size,
            mode: 0o644,
            modified,
            created: modified,
        },
    }
}

fn std_metadata(meta: &Meta) -> Metadata {
    let file_type = match meta.kind {
        EntryType::Symlink => FileType::Symlink,
        EntryType::Dir => FileType::Directory,
        EntryType::Fifo => FileType::Fifo,
        EntryType::File | EntryType::Other => FileType::File,
    };
    Metadata {
        file_type,
        size: meta.size,
        mode: meta.mode,
        modified: meta.modified,
        created: meta.modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::Quota;
    use crate::volume::LocalVolume;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("faber-vfs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scratch_with(dir: &Path, binds: Vec<Bind>) -> ScratchFs {
        let base = dir.join("base");
        fs::create_dir_all(base.join("etc")).unwrap();
        let layer = Arc::new(Layer::open(dir.join("layer")).unwrap());
        let usage = Arc::new(Quota::new(None)).track("user", "session", 0);
        ScratchFs::new(base, binds, layer, Arc::default(), Arc::default(), usage)
    }

    fn scratch(dir: &Path) -> ScratchFs {
        scratch_with(dir, Vec::new())
    }

    #[tokio::test]
    async fn a_symlink_in_the_layer_resolves_inside_scratch() {
        let dir = temp("resolve");
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let fs = scratch(&dir);
        // A link whose target is a real host path is a scratch path too.
        fs.symlink(&outside, Path::new("/d")).await.unwrap();
        // Nothing is there in scratch yet, whatever the host has.
        assert!(fs.write_file(Path::new("/d/file"), b"x").await.is_err());
        fs.mkdir(&outside, true).await.unwrap();
        fs.write_file(Path::new("/d/file"), b"x").await.unwrap();

        assert!(!outside.join("file").exists());
        let inside = format!("{}/file", outside.display());
        assert_eq!(fs.read_file(Path::new(&inside)).await.unwrap(), b"x");
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_directory_turned_symlink_after_resolving_is_never_followed() {
        let dir = temp("swap");
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), b"host").unwrap();
        let fs = scratch(&dir);
        // What another run could leave between this one resolving `/d/..`
        // as a directory and using it: `/d` is now a link out of the layer.
        symlink(&outside, fs.layer.upper().join("d")).unwrap();

        let error = fs.write_tree("/d/new", b"x", false).await.unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
        assert!(!outside.join("new").exists());
        assert!(fs.lookup("/d/secret").await.unwrap().is_none());
        assert!(fs.mkdir_tree("/d/sub").await.is_err());
        assert!(!outside.join("sub").exists());
        assert!(fs.list_tree("/d").await.unwrap().is_empty());
        assert!(fs.remove_tree("/d/secret").await.is_ok());
        assert_eq!(fs::read(outside.join("secret")).unwrap(), b"host");
        fs::remove_dir_all(dir).unwrap();
    }

    /// A link inside a volume resolves in scratch, as in the container that
    /// binds the volume: an absolute one leads out of the volume to the
    /// image, never to the host.
    #[tokio::test]
    async fn a_symlink_in_a_volume_resolves_in_scratch() {
        let dir = temp("bind-link");
        let workspace = dir.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let fs = scratch_with(
            &dir,
            vec![Bind {
                at: "/workspace".to_owned(),
                volume: Arc::new(LocalVolume::new(&workspace)),
                read_only: false,
            }],
        );
        fs::write(dir.join("base/etc/hostname"), b"image").unwrap();
        fs.symlink(Path::new("/etc"), Path::new("/workspace/etc"))
            .await
            .unwrap();
        assert_eq!(
            fs.read_file(Path::new("/workspace/etc/hostname"))
                .await
                .unwrap(),
            b"image"
        );
        assert!(fs.write_file(Path::new("/workspace/a/b"), b"x").await.is_err());
        fs.mkdir(Path::new("/workspace/a"), false).await.unwrap();
        fs.write_file(Path::new("/workspace/a/b"), b"x")
            .await
            .unwrap();
        assert_eq!(fs::read(workspace.join("a/b")).unwrap(), b"x");
        fs.append_file(Path::new("/workspace/a/b"), b"y")
            .await
            .unwrap();
        fs.chmod(Path::new("/workspace/a/b"), 0o600).await.unwrap();
        let meta = fs.stat(Path::new("/workspace/a/b")).await.unwrap();
        assert_eq!((meta.size, meta.mode), (2, 0o600));
        fs.rename(Path::new("/workspace/a/b"), Path::new("/workspace/c"))
            .await
            .unwrap();
        let error = fs
            .rename(Path::new("/workspace/c"), Path::new("/c"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cross-device"), "{error}");
        fs.remove(Path::new("/workspace/a"), true).await.unwrap();
        assert!(!workspace.join("a").exists());
        assert!(workspace.join("c").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
