//! Scratch's `/` as virtual mode sees it: the read-only base image under the
//! session's layer, the toolstore, and the mounts.
//!
//! This is the filesystem the virtual shell runs over, and the one `read` and
//! `patch` use while scratch is virtual. It answers the way the sandbox's
//! overlay would, because compute mode sees the same base and the same layer:
//!
//! - Lookups go to the layer first, then to the base, unless the layer deleted
//!   the path (a whiteout) or replaced the directory above it wholesale (an
//!   opaque directory).
//! - Writes copy up into the layer. The base directory is never written.
//! - Symlinks are resolved here, component by component, inside scratch's own
//!   namespace. The base image is full of absolute symlinks (`/bin` is one on
//!   Debian); following them through the host would leave the image.
//! - Every host operation reaches its file through directories opened without
//!   following symlinks ([`Beneath`]), and a path is resolved and used under
//!   one hold of the change lock. Two runs in the same session share the
//!   layer, so one could otherwise swap a directory for a symlink between the
//!   other resolving a path and the host following it out of the layer.
//! - `/opt/toolstore` is the operator's toolstore, read-only, as compute mode
//!   binds it.
//! - `/mnt` belongs to the mounts, whatever the base has there.
//!
//! [`stat`](FileSystem::stat) follows a final symlink, as `stat(2)` does, so
//! `[ -d /bin ]` and `command -v` agree with the sandbox; the shell commands
//! that would need `lstat` are not run virtually.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bashkit::{DirEntry, FileSystem, FileSystemExt, FileType, Metadata};

use crate::beneath::{self, Beneath};
use crate::layer::{Layer, ancestors, is_at_or_under};
use crate::mount::{
    self, ErrorLog, FsError, Kind, MNT, MntPath, Mount, MountTable, classify, read_all,
};
use crate::quota::Usage;

/// Where the operator's toolstore appears in scratch.
pub const TOOLSTORE: &str = "/opt/toolstore";

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

/// What a resolved path names: a place under `/mnt`, or what an operation
/// on the tree returned.
enum At<T> {
    Mnt(MntPath),
    Tree(T),
}

/// An entry found in the tree, with where it really is.
struct Found {
    real: Beneath,
    in_layer: bool,
    meta: fs::Metadata,
}

impl Found {
    fn is_dir(&self) -> bool {
        self.meta.is_dir()
    }

    fn is_symlink(&self) -> bool {
        self.meta.file_type().is_symlink()
    }
}

/// Scratch's merged filesystem for one session.
pub struct ScratchFs {
    base: PathBuf,
    toolstore: Option<PathBuf>,
    layer: Arc<Layer>,
    mounts: Arc<MountTable>,
    errors: Arc<ErrorLog>,
    usage: Usage,
    /// The version a mount file had when this session last read it, so the
    /// next whole-file write of it is a conditional one and a concurrent edit
    /// surfaces as a conflict rather than being overwritten.
    versions: Mutex<HashMap<String, String>>,
    /// Held to read the tree, and held exclusively to change it, from
    /// resolving a path to the last host operation on it: a copy-up and the
    /// marks that go with it are never interleaved with another change, and
    /// nothing changes between resolving a path and using it.
    changing: RwLock<()>,
}

impl ScratchFs {
    pub fn new(
        base: PathBuf,
        toolstore: Option<PathBuf>,
        layer: Arc<Layer>,
        mounts: Arc<MountTable>,
        errors: Arc<ErrorLog>,
        usage: Usage,
    ) -> Self {
        ScratchFs {
            base,
            toolstore,
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

    /// The toolstore and the path inside it, when `path` is in the toolstore.
    fn toolstore_at<'a>(&self, path: &'a str) -> Option<(&Path, &'a str)> {
        let toolstore = self.toolstore.as_deref()?;
        if !is_at_or_under(path, TOOLSTORE) {
            return None;
        }
        Some((toolstore, &path[TOOLSTORE.len()..]))
    }

    fn in_toolstore(&self, path: &str) -> bool {
        self.toolstore_at(path).is_some()
    }

    /// Where `path` is in the layer's tree, to be created or changed.
    fn upper(&self, path: &str) -> io::Result<Beneath> {
        Beneath::new(self.layer.upper(), path)
    }

    /// What is at `path` in the tree, without following a final symlink.
    fn lookup(&self, path: &str) -> io::Result<Option<Found>> {
        if let Some((root, rest)) = self.toolstore_at(path) {
            return found(root, rest, false);
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
        if self.layer.under_opaque(path) || self.in_toolstore(path) {
            return Ok(false);
        }
        Ok(found(&self.base, path, false)?.is_some())
    }

    /// Resolves `path` as [`resolve`](Self::resolve) does, holding the tree
    /// still while it does.
    pub(crate) fn resolve_now(
        &self,
        path: &str,
        follow: bool,
        missing: bool,
    ) -> io::Result<String> {
        let _reading = self.reading();
        self.resolve(path, follow, missing)
    }

    fn reading(&self) -> std::sync::RwLockReadGuard<'_, ()> {
        self.changing.read().expect("scratch fs poisoned")
    }

    fn writing(&self) -> std::sync::RwLockWriteGuard<'_, ()> {
        self.changing.write().expect("scratch fs poisoned")
    }

    /// Resolves `path` (absolute) to its canonical scratch path, following
    /// symlinks in every component but the last, and the last too when
    /// `follow` is set. A missing last component is allowed when `missing` is
    /// set, which is how a create finds where it would land.
    pub(crate) fn resolve(&self, path: &str, follow: bool, missing: bool) -> io::Result<String> {
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
            match self.lookup(&candidate)? {
                None if last && missing => done.push(part),
                None => return fail(libc::ENOENT),
                Some(found) if found.is_symlink() && (!last || follow) => {
                    hops += 1;
                    if hops > MAX_SYMLINKS {
                        return fail(libc::ELOOP);
                    }
                    let target = fs::read_link(&found.real)?;
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

    /// Resolves `path` and, when it lands in the tree, runs `op` on the
    /// resolved path without letting go of the change lock in between —
    /// exclusively when `write` is set. A path under `/mnt` comes back with
    /// the lock released, for the provider call that follows.
    fn at<T>(
        &self,
        path: &Path,
        follow: bool,
        missing: bool,
        write: bool,
        op: impl FnOnce(&str) -> io::Result<T>,
    ) -> io::Result<At<T>> {
        let path = path.to_str().ok_or_else(|| errno_error(libc::EINVAL))?;
        if !path.starts_with('/') {
            return fail(libc::EINVAL);
        }
        let (_reading, _writing) = if write {
            (None, Some(self.writing()))
        } else {
            (Some(self.reading()), None)
        };
        let resolved = self.resolve(path, follow, missing)?;
        match classify(&resolved) {
            Some(mnt) => Ok(At::Mnt(mnt)),
            None => op(&resolved).map(At::Tree),
        }
    }

    /// Resolves `path` with the change lock already held: a place under
    /// `/mnt`, or the resolved path in the tree.
    fn place(
        &self,
        path: &Path,
        follow: bool,
        missing: bool,
    ) -> io::Result<(Option<MntPath>, Option<String>)> {
        let path = path.to_str().ok_or_else(|| errno_error(libc::EINVAL))?;
        if !path.starts_with('/') {
            return fail(libc::EINVAL);
        }
        let resolved = self.resolve(path, follow, missing)?;
        Ok(match classify(&resolved) {
            Some(mnt) => (Some(mnt), None),
            None => (None, Some(resolved)),
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
    fn list_tree(&self, path: &str) -> io::Result<BTreeMap<String, fs::Metadata>> {
        let mut names = BTreeMap::new();
        if let Some((root, rest)) = self.toolstore_at(path) {
            read_names(root, rest, &mut names)?;
            return Ok(names);
        }
        read_names(self.layer.upper(), path, &mut names)?;
        if !self.layer.is_opaque(path) && !self.layer.under_opaque(path) {
            let mut base = BTreeMap::new();
            read_names(&self.base, path, &mut base)?;
            for (name, meta) in base {
                let child = child_of(path, &name);
                if !names.contains_key(&name) && !self.layer.is_whiteout(&child) {
                    names.insert(name, meta);
                }
            }
        }
        if path == "/" {
            names.remove("mnt");
            if let Ok(meta) = fs::symlink_metadata(&self.base) {
                names.insert("mnt".to_owned(), meta);
            }
        }
        if path == "/opt"
            && let Some(toolstore) = &self.toolstore
            && let Ok(meta) = fs::symlink_metadata(toolstore)
        {
            names.insert("toolstore".to_owned(), meta);
        }
        Ok(names)
    }

    /// Makes sure the directory at `path` exists in the layer, creating it
    /// and its ancestors with the modes they have in the merged view.
    fn ensure_dir(&self, path: &str) -> io::Result<()> {
        let mut chain: Vec<&str> = ancestors(path).collect();
        chain.reverse();
        chain.push(path);
        for dir in chain {
            if dir == "/" {
                continue;
            }
            let real = self.upper(dir)?;
            if lstat(&real)?.is_some() {
                continue;
            }
            let mode = match self.lookup(dir)? {
                Some(found) if found.is_dir() => found.meta.mode() & 0o7777,
                Some(_) => return fail(libc::ENOTDIR),
                None => 0o755,
            };
            fs::create_dir(&real)?;
            beneath::set_mode(&real, mode | 0o700)?;
        }
        Ok(())
    }

    /// Brings a base entry into the layer so it can be changed in place.
    fn copy_up(&self, path: &str, found: &Found) -> io::Result<()> {
        if found.in_layer {
            return Ok(());
        }
        if self.in_toolstore(path) {
            return fail(libc::EROFS);
        }
        if found.is_dir() {
            return self.ensure_dir(path);
        }
        self.ensure_dir(parent_of(path))?;
        let real = self.upper(path)?;
        if found.is_symlink() {
            symlink(fs::read_link(&found.real)?, &real)?;
            return Ok(());
        }
        self.usage
            .change(0, found.meta.len())
            .map_err(|_| errno_error(libc::EDQUOT))?;
        beneath::copy_file(&found.real, &real)?;
        Ok(())
    }

    /// Writes a regular file in the tree, copying it up first.
    fn write_tree(&self, path: &str, content: &[u8], append: bool) -> io::Result<()> {
        if self.in_toolstore(path) {
            return fail(libc::EROFS);
        }
        let existing = self.lookup(path)?;
        let mode = match &existing {
            Some(found) if found.is_dir() => return fail(libc::EISDIR),
            Some(found) => {
                if append {
                    self.copy_up(path, found)?;
                } else {
                    self.ensure_dir(parent_of(path))?;
                }
                found.meta.mode() & 0o7777
            }
            None => {
                self.ensure_dir(parent_of(path))?;
                0o644
            }
        };
        let real = self.upper(path)?;
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

    fn mkdir_tree(&self, path: &str) -> io::Result<()> {
        if self.in_toolstore(path) {
            return fail(libc::EROFS);
        }
        if self.lookup(path)?.is_some() {
            return fail(libc::EEXIST);
        }
        self.ensure_dir(parent_of(path))?;
        fs::create_dir(self.upper(path)?)?;
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
    fn remove_tree(&self, path: &str) -> io::Result<()> {
        if self.in_toolstore(path) {
            return fail(libc::EROFS);
        }
        if path == "/" {
            return fail(libc::EBUSY);
        }
        let in_base = self.base_has(path)?;
        if let Some(Found { real, meta, .. }) = found(self.layer.upper(), path, true)? {
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
            self.ensure_dir(parent_of(path))?;
            self.layer.add_whiteout(path)?;
        }
        Ok(())
    }

    /// Copies the merged view of `from` into the layer at `to`, where
    /// nothing is. A copy that fails part way — the quota, above all — is
    /// removed again, so a failed `mv` leaves no half of it behind.
    fn copy_tree(&self, from: &str, to: &str) -> io::Result<()> {
        let copied = self.copy_entries(from, to);
        if copied.is_err()
            && let Ok(Some(Found { real, meta, .. })) = found(self.layer.upper(), to, true)
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

    fn copy_entries(&self, from: &str, to: &str) -> io::Result<()> {
        let Some(found) = self.lookup(from)? else {
            return fail(libc::ENOENT);
        };
        let target = self.upper(to)?;
        if found.is_symlink() {
            symlink(fs::read_link(&found.real)?, &target)?;
        } else if found.is_dir() {
            fs::create_dir(&target)?;
            beneath::set_mode(&target, (found.meta.mode() & 0o7777) | 0o700)?;
            for name in self.list_tree(from)?.keys() {
                self.copy_entries(&child_of(from, name), &child_of(to, name))?;
            }
        } else {
            self.usage
                .change(0, found.meta.len())
                .map_err(|_| errno_error(libc::EDQUOT))?;
            beneath::copy_file(&found.real, &target)?;
        }
        Ok(())
    }

    fn rename_tree(&self, from: &str, to: &str) -> io::Result<()> {
        if self.in_toolstore(from) || self.in_toolstore(to) {
            return fail(libc::EROFS);
        }
        if from == to {
            return Ok(());
        }
        if is_at_or_under(to, from) {
            return fail(libc::EINVAL);
        }
        let Some(source) = self.lookup(from)? else {
            return fail(libc::ENOENT);
        };
        match self.lookup(to)? {
            Some(dest) if dest.is_dir() && !source.is_dir() => return fail(libc::EISDIR),
            Some(dest) if !dest.is_dir() && source.is_dir() => return fail(libc::ENOTDIR),
            Some(dest) if dest.is_dir() && !self.list_tree(to)?.is_empty() => {
                return fail(libc::ENOTEMPTY);
            }
            Some(_) => self.remove_tree(to)?,
            None => {}
        }
        self.ensure_dir(parent_of(to))?;
        let dest_base = self.base_has(to)? || self.layer.is_whiteout(to);
        let merged = source.is_dir() && self.base_has(from)?;
        if source.in_layer && !merged {
            fs::rename(&source.real, self.upper(to)?)?;
            self.layer.rename_marks(from, to)?;
        } else {
            self.copy_tree(from, to)?;
        }
        self.layer.clear_whiteout(to)?;
        if source.is_dir() && dest_base {
            self.layer.set_opaque(to)?;
        }
        if found(self.layer.upper(), from, true)?.is_some() || self.base_has(from)? {
            self.remove_tree(from)?;
        }
        Ok(())
    }

    /// The bytes and metadata of a resolved tree path, following the final
    /// symlink. For `read` and `patch`, which address files, not links.
    pub(crate) fn read_tree(&self, path: &str) -> io::Result<Vec<u8>> {
        match self.lookup(path)? {
            None => fail(libc::ENOENT),
            Some(found) if found.is_dir() => fail(libc::EISDIR),
            Some(found) => {
                let mut bytes = Vec::new();
                io::Read::read_to_end(&mut beneath::open_nofollow(&found.real)?, &mut bytes)?;
                Ok(bytes)
            }
        }
    }
}

#[async_trait]
impl FileSystem for ScratchFs {
    async fn read_file(&self, path: &Path) -> bashkit::Result<Vec<u8>> {
        Ok(
            match self.at(path, true, false, false, |path| self.read_tree(path))? {
                At::Mnt(MntPath::Errors) => self.errors.render(),
                At::Mnt(MntPath::Inside { plugin, name, path }) => {
                    self.mount_read(&plugin, &name, &path).await?
                }
                At::Mnt(_) => return Err(errno_error(libc::EISDIR).into()),
                At::Tree(bytes) => bytes,
            },
        )
    }

    async fn write_file(&self, path: &Path, content: &[u8]) -> bashkit::Result<()> {
        match self.at(path, true, true, true, |path| {
            self.write_tree(path, content, false)
        })? {
            At::Mnt(MntPath::Inside { plugin, name, path }) => {
                if path == "/" {
                    return Err(errno_error(libc::EISDIR).into());
                }
                self.mount_write(&plugin, &name, &path, content.to_vec())
                    .await?;
                Ok(())
            }
            At::Mnt(MntPath::Errors) => Err(errno_error(libc::EACCES).into()),
            At::Mnt(_) => Err(errno_error(libc::EISDIR).into()),
            At::Tree(()) => Ok(()),
        }
    }

    async fn append_file(&self, path: &Path, content: &[u8]) -> bashkit::Result<()> {
        match self.at(path, true, true, true, |path| {
            self.write_tree(path, content, true)
        })? {
            At::Mnt(MntPath::Inside { plugin, name, path }) => {
                let mut bytes = match self.mount_read(&plugin, &name, &path).await {
                    Ok(bytes) => bytes,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
                    Err(error) => return Err(error.into()),
                };
                bytes.extend_from_slice(content);
                self.mount_write(&plugin, &name, &path, bytes).await?;
                Ok(())
            }
            At::Mnt(MntPath::Errors) => Err(errno_error(libc::EACCES).into()),
            At::Mnt(_) => Err(errno_error(libc::EISDIR).into()),
            At::Tree(()) => Ok(()),
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
        match self.at(path, false, true, true, |path| self.mkdir_tree(path))? {
            At::Mnt(MntPath::Inside { plugin, name, path }) => {
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
            At::Mnt(_) => Err(errno_error(libc::EEXIST).into()),
            At::Tree(()) => Ok(()),
        }
    }

    async fn remove(&self, path: &Path, recursive: bool) -> bashkit::Result<()> {
        let removed = self.at(path, false, false, true, |path| {
            let Some(found) = self.lookup(path)? else {
                return fail(libc::ENOENT);
            };
            if found.is_dir() && !recursive && !self.list_tree(path)?.is_empty() {
                return fail(libc::ENOTEMPTY);
            }
            self.remove_tree(path)
        })?;
        match removed {
            At::Mnt(MntPath::Inside { plugin, name, path }) => {
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
            At::Mnt(_) => Err(errno_error(libc::EBUSY).into()),
            At::Tree(()) => Ok(()),
        }
    }

    async fn stat(&self, path: &Path) -> bashkit::Result<Metadata> {
        let looked = self.at(path, true, false, false, |path| {
            match self.lookup(path)? {
                Some(found) => Ok(std_metadata(&found.meta)),
                None => fail(libc::ENOENT),
            }
        })?;
        Ok(match looked {
            At::Mnt(MntPath::Inside { plugin, name, path }) => {
                let mount = self.mount(&plugin, &name)?;
                let entry = self
                    .provider("stat", &plugin, &name, &path, mount.backend.stat(&path))
                    .await?;
                entry_metadata(&entry)
            }
            At::Mnt(mnt) => self.mnt_metadata(&mnt)?,
            At::Tree(meta) => meta,
        })
    }

    async fn read_dir(&self, path: &Path) -> bashkit::Result<Vec<DirEntry>> {
        let listed = self.at(path, true, false, false, |path| {
            match self.lookup(path)? {
                Some(found) if found.is_dir() => {}
                Some(_) => return fail(libc::ENOTDIR),
                None => return fail(libc::ENOENT),
            }
            Ok(self
                .list_tree(path)?
                .into_iter()
                .map(|(name, meta)| DirEntry {
                    name,
                    metadata: std_metadata(&meta),
                })
                .collect::<Vec<_>>())
        })?;
        Ok(match listed {
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
            At::Tree(entries) => entries,
        })
    }

    async fn exists(&self, path: &Path) -> bashkit::Result<bool> {
        Ok(self.stat(path).await.is_ok())
    }

    async fn rename(&self, from: &Path, to: &Path) -> bashkit::Result<()> {
        // Both ends are resolved and the move made under one hold of the
        // change lock.
        let places = {
            let _writing = self.writing();
            let source = self.place(from, false, false)?;
            let dest = self.place(to, false, true)?;
            if let (Some(from), Some(to)) = (&source.1, &dest.1) {
                self.rename_tree(from, to)?;
                return Ok(());
            }
            (source.0, dest.0)
        };
        match places {
            (
                Some(MntPath::Inside {
                    plugin,
                    name,
                    path: src,
                }),
                Some(MntPath::Inside {
                    plugin: to_plugin,
                    name: to_name,
                    path: dst,
                }),
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
        let made = self.at(link, false, true, true, |path| {
            if self.in_toolstore(path) {
                return fail(libc::EROFS);
            }
            if self.lookup(path)?.is_some() {
                return fail(libc::EEXIST);
            }
            self.ensure_dir(parent_of(path))?;
            symlink(target, self.upper(path)?)?;
            self.layer.clear_whiteout(path)
        })?;
        match made {
            At::Mnt(_) => Err(errno_error(libc::EPERM).into()),
            At::Tree(()) => Ok(()),
        }
    }

    async fn read_link(&self, path: &Path) -> bashkit::Result<PathBuf> {
        let read = self.at(path, false, false, false, |path| {
            match self.lookup(path)? {
                Some(found) if found.is_symlink() => fs::read_link(&found.real),
                Some(_) => fail(libc::EINVAL),
                None => fail(libc::ENOENT),
            }
        })?;
        match read {
            At::Mnt(_) => Err(errno_error(libc::EINVAL).into()),
            At::Tree(target) => Ok(target),
        }
    }

    async fn chmod(&self, path: &Path, mode: u32) -> bashkit::Result<()> {
        // Mounts have no modes; accepting the change keeps `chmod` in a
        // script from failing on them, as it does under FUSE.
        self.at(path, true, false, true, |path| {
            let Some(found) = self.lookup(path)? else {
                return fail(libc::ENOENT);
            };
            self.copy_up(path, &found)?;
            beneath::set_mode(&self.upper(path)?, mode & 0o7777)
        })?;
        Ok(())
    }

    async fn set_modified_time(&self, path: &Path, time: SystemTime) -> bashkit::Result<()> {
        self.at(path, true, false, true, |path| {
            let Some(found) = self.lookup(path)? else {
                return fail(libc::ENOENT);
            };
            self.copy_up(path, &found)?;
            beneath::open_nofollow(&self.upper(path)?)?.set_modified(time)
        })?;
        Ok(())
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
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// What is at `relative` under `root`, without following a symlink anywhere
/// on the way. A directory on the way that is missing, or is not a directory,
/// means nothing is there.
fn found(root: &Path, relative: &str, in_layer: bool) -> io::Result<Option<Found>> {
    let real = match Beneath::new(root, relative) {
        Ok(real) => real,
        Err(error) if gone(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(lstat(&real)?.map(|meta| Found {
        real,
        in_layer,
        meta,
    }))
}

fn gone(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Adds the entries of the directory at `relative` under `root` to `into`.
fn read_names(
    root: &Path,
    relative: &str,
    into: &mut BTreeMap<String, fs::Metadata>,
) -> io::Result<()> {
    let real = match Beneath::new(root, relative) {
        Ok(real) => real,
        Err(error) if gone(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let dir = match beneath::open_dir(&real) {
        Ok(dir) => dir,
        // A final symlink is not a directory to list.
        Err(error) if gone(&error) || error.raw_os_error() == Some(libc::ELOOP) => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Ok(meta) = fs::symlink_metadata(entry.path()) {
            into.insert(name, meta);
        }
    }
    Ok(())
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

fn std_metadata(meta: &fs::Metadata) -> Metadata {
    let file_type = if meta.file_type().is_symlink() {
        FileType::Symlink
    } else if meta.is_dir() {
        FileType::Directory
    } else if meta.file_type().is_file() {
        FileType::File
    } else {
        use std::os::unix::fs::FileTypeExt;
        if meta.file_type().is_fifo() {
            FileType::Fifo
        } else {
            FileType::File
        }
    };
    let modified = meta.modified().unwrap_or(UNIX_EPOCH);
    Metadata {
        file_type,
        size: meta.len(),
        mode: meta.mode() & 0o7777,
        modified,
        created: modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::Quota;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("faber-vfs-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scratch(dir: &Path) -> ScratchFs {
        let base = dir.join("base");
        fs::create_dir_all(base.join("etc")).unwrap();
        let layer = Arc::new(Layer::open(dir.join("layer")).unwrap());
        let usage = Arc::new(Quota::new(None)).track("user", "session", 0);
        ScratchFs::new(base, None, layer, Arc::default(), Arc::default(), usage)
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

    #[test]
    fn a_directory_turned_symlink_after_resolving_is_never_followed() {
        let dir = temp("swap");
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), b"host").unwrap();
        let fs = scratch(&dir);
        // What another run could leave between this one resolving `/d/..`
        // as a directory and using it: `/d` is now a link out of the layer.
        symlink(&outside, fs.layer.upper().join("d")).unwrap();

        let error = fs.write_tree("/d/new", b"x", false).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
        assert!(!outside.join("new").exists());
        assert!(fs.lookup("/d/secret").unwrap().is_none());
        assert!(fs.mkdir_tree("/d/sub").is_err());
        assert!(!outside.join("sub").exists());
        assert!(fs.list_tree("/d").unwrap().is_empty());
        assert!(fs.remove_tree("/d/secret").is_ok());
        assert_eq!(fs::read(outside.join("secret")).unwrap(), b"host");
        fs::remove_dir_all(dir).unwrap();
    }
}
