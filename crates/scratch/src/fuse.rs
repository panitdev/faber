//! `/mnt`: one FUSE server answering for every mount.
//!
//! tenantd mounts the container's FUSE slot at `/mnt`, speaks the kernel
//! protocol itself and forwards path-based operations over
//! `Containers.ServeFuse` to this process, which answers them from the
//! session's [`MountTable`]. tenantd never runs filesystem code, and nothing
//! here depends on being on tenantd's host: the stream is the whole contract.
//!
//! Every request is dispatched through the table and every attribute goes
//! out with a zero TTL, so a mount added or removed while the container runs
//! is visible on the next access.
//!
//! Writes are buffered per open file and sent to the provider as one whole-
//! file `put` on `flush` (every `close`), conditional on the version the file
//! had when it was opened. A refused `put` — a conflict above all — reaches
//! the program as the errno of its `close`, and the real error is logged to
//! [`ERRORS_PATH`](crate::mount::ERRORS_PATH), which this server also serves.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tenantd_client::fuse::{Context, Error, Filesystem, Result};
use tenantd_client::pb;

use crate::mount::{
    ERRORS_NAME, ErrorLog, FsError, Kind, MAX_FILE, MNT, MntPath, Mount, MountTable, classify,
    read_all,
};

/// Whether `path` is `root` or below it.
fn is_at_or_under(path: &str, root: &str) -> bool {
    root == "/"
        || path == root
        || (path.len() > root.len()
            && path.starts_with(root)
            && path.as_bytes()[root.len()] == b'/')
}

/// The largest write tenantd may send in one request; the server holds a
/// whole request.
pub const MAX_WRITE: u32 = 1 << 20;

/// What all of a session's open handles may buffer together. One sandbox
/// cannot hold more of the Faber process's memory than this, however many
/// files it opens.
const MAX_BUFFERED: u64 = 1 << 30;

/// An open file and what has been written to it.
struct Open {
    path: String,
    /// The whole file as this handle will put it, once anything was written.
    buffer: Option<Vec<u8>>,
    dirty: bool,
    /// The version the file had when it was opened or last put.
    version: Option<String>,
    /// Opened with `O_APPEND`: every write goes to the end, whatever offset
    /// the request carries.
    append: bool,
    /// Made by `create` and not yet put: `Some(exclusive)`. The file is put
    /// once, with its contents, when the handle is flushed.
    creating: Option<bool>,
}

/// The FUSE server for one session's `/mnt`.
pub struct MntFs {
    mounts: Arc<MountTable>,
    errors: Arc<ErrorLog>,
    handles: Mutex<HashMap<u64, Open>>,
    next_handle: AtomicU64,
}

fn errno(code: i32) -> Error {
    Error::new(code)
}

/// The scratch path of a path in the exported tree, whose root is `/mnt`.
fn scratch_path(path: &str) -> Result<String> {
    if !path.starts_with('/') {
        return Err(errno(libc::EINVAL));
    }
    if path.split('/').any(|part| part == "." || part == "..") {
        return Err(errno(libc::EINVAL));
    }
    let trimmed = path.trim_end_matches('/');
    Ok(format!("{MNT}{trimmed}"))
}

fn attr_of(kind: Kind, size: u64, mtime: u64, read_only: bool) -> pb::FileAttr {
    let time = (mtime as i64).saturating_mul(1_000_000_000);
    let (kind, mode, nlink) = match kind {
        Kind::Dir => (pb::FileKind::Directory, 0o755, 2),
        Kind::File if read_only => (pb::FileKind::File, 0o444, 1),
        Kind::File => (pb::FileKind::File, 0o644, 1),
    };
    pb::FileAttr {
        kind: kind as i32,
        mode,
        size,
        nlink,
        atime_ns: time,
        mtime_ns: time,
        ctime_ns: time,
        // Nothing is cached in the sandbox: the table and the providers are
        // the truth, and a mount added a moment ago must resolve.
        ttl_ms: 0,
        ..Default::default()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl MntFs {
    pub fn new(mounts: Arc<MountTable>, errors: Arc<ErrorLog>) -> Self {
        MntFs {
            mounts,
            errors,
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        }
    }

    /// What this server can take, for [`tenantd_client::fuse::attach`].
    pub fn limits() -> pb::FuseLimits {
        pb::FuseLimits {
            max_write: MAX_WRITE,
            ..Default::default()
        }
    }

    fn mount(&self, plugin: &str, name: &str) -> Result<Mount> {
        self.mounts.get(plugin, name).ok_or_else(Error::not_found)
    }

    /// The mount and mount-relative path of a scratch path inside a mount.
    fn inside(&self, path: &str) -> Result<(Mount, String)> {
        match classify(path) {
            Some(MntPath::Inside { plugin, name, path }) => Ok((self.mount(&plugin, &name)?, path)),
            Some(MntPath::Errors) => Err(errno(libc::EACCES)),
            _ => Err(errno(libc::EISDIR)),
        }
    }

    /// Like [`inside`](Self::inside), refusing a read-only mount before the
    /// provider is asked, and logging it.
    fn writable(&self, op: &str, path: &str) -> Result<(Mount, String)> {
        let (mount, inner) = match classify(path) {
            Some(MntPath::Inside { .. }) => self.inside(path)?,
            // `/mnt` and `/mnt/<plugin>` belong to the table, not a provider.
            _ => return Err(errno(libc::EACCES)),
        };
        if mount.read_only {
            self.errors.record(op, path, FsError::ReadOnly);
            return Err(errno(FsError::ReadOnly.errno()));
        }
        Ok((mount, inner))
    }

    /// Awaits a provider call, logging any error but a plain miss.
    async fn call<T>(
        &self,
        op: &str,
        path: &str,
        future: impl std::future::Future<Output = std::result::Result<T, FsError>>,
    ) -> Result<T> {
        future.await.map_err(|error| {
            if error != FsError::NotFound {
                self.errors.record(op, path, error);
            }
            errno(error.errno()).detail(format!("{op} {path}: {error:?}"))
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Open>> {
        self.handles.lock().expect("handles poisoned")
    }

    /// The size an open, written handle gives a path, which the provider does
    /// not know yet.
    fn buffered_size(&self, path: &str) -> Option<u64> {
        self.lock()
            .values()
            .filter(|open| open.path == path && open.dirty)
            .find_map(|open| open.buffer.as_ref().map(|buffer| buffer.len() as u64))
    }

    /// The size of a file this session created and has not put yet, which
    /// the provider does not know of.
    fn creating_size(&self, path: &str) -> Option<u64> {
        self.lock()
            .values()
            .find(|open| open.path == path && open.creating.is_some())
            .map(|open| open.buffer.as_ref().map_or(0, |buffer| buffer.len() as u64))
    }

    /// A scratch path's attributes.
    async fn attr(&self, path: &str) -> Result<pb::FileAttr> {
        match classify(path).ok_or_else(Error::not_found)? {
            MntPath::Root => Ok(attr_of(Kind::Dir, 0, 0, false)),
            MntPath::Errors => {
                let size = self.errors.render().len() as u64;
                Ok(attr_of(Kind::File, size, now(), true))
            }
            MntPath::Plugin(plugin) => {
                if self.mounts.plugins().contains(&plugin) {
                    Ok(attr_of(Kind::Dir, 0, 0, false))
                } else {
                    Err(Error::not_found())
                }
            }
            MntPath::Inside {
                plugin,
                name,
                path: inner,
            } => {
                let mount = self.mount(&plugin, &name)?;
                if let Some(size) = self.creating_size(path) {
                    return Ok(attr_of(Kind::File, size, now(), mount.read_only));
                }
                let entry = self.call("stat", path, mount.backend.stat(&inner)).await?;
                let size = self.buffered_size(path).unwrap_or(entry.size);
                Ok(attr_of(entry.kind, size, entry.mtime, mount.read_only))
            }
        }
    }

    /// Children of a directory with their attributes.
    async fn list(&self, path: &str) -> Result<Vec<pb::DirEntry>> {
        let dir = |name: String| pb::DirEntry {
            name,
            attr: Some(attr_of(Kind::Dir, 0, 0, false)),
        };
        match classify(path).ok_or_else(Error::not_found)? {
            MntPath::Root => {
                let mut entries: Vec<pb::DirEntry> =
                    self.mounts.plugins().into_iter().map(dir).collect();
                entries.push(pb::DirEntry {
                    name: ERRORS_NAME.to_owned(),
                    attr: Some(attr_of(
                        Kind::File,
                        self.errors.render().len() as u64,
                        now(),
                        true,
                    )),
                });
                Ok(entries)
            }
            MntPath::Plugin(plugin) => {
                let names = self.mounts.names(&plugin);
                if names.is_empty() {
                    return Err(Error::not_found());
                }
                Ok(names.into_iter().map(dir).collect())
            }
            MntPath::Errors => Err(errno(libc::ENOTDIR)),
            MntPath::Inside {
                plugin,
                name,
                path: inner,
            } => {
                let mount = self.mount(&plugin, &name)?;
                let entries = self
                    .call("read-dir", path, mount.backend.read_dir(&inner))
                    .await?;
                Ok(entries
                    .into_iter()
                    .map(|entry| pb::DirEntry {
                        attr: Some(attr_of(
                            entry.kind,
                            entry.size,
                            entry.mtime,
                            mount.read_only,
                        )),
                        name: entry.name,
                    })
                    .collect())
            }
        }
    }

    /// Whether the handles may grow by `more` bytes, all together.
    fn room(handles: &HashMap<u64, Open>, more: u64) -> Result<()> {
        let held: u64 = handles
            .values()
            .map(|open| open.buffer.as_ref().map_or(0, |buffer| buffer.len() as u64))
            .sum();
        if held.saturating_add(more) > MAX_BUFFERED {
            return Err(errno(libc::ENOSPC));
        }
        Ok(())
    }

    /// Makes sure a handle holds the whole file, reading it on first need.
    async fn load(&self, fh: u64) -> Result<()> {
        let path = {
            let handles = self.lock();
            let open = handles.get(&fh).ok_or_else(|| errno(libc::EBADF))?;
            if open.buffer.is_some() {
                return Ok(());
            }
            open.path.clone()
        };
        let (mount, inner) = self.inside(&path)?;
        let bytes = self
            .call("read", &path, read_all(mount.backend.as_ref(), &inner))
            .await?;
        tracing::trace!(target: "scratch::fuse", fh, loaded = bytes.len(), "load");
        let mut handles = self.lock();
        Self::room(&handles, bytes.len() as u64)?;
        let open = handles.get_mut(&fh).ok_or_else(|| errno(libc::EBADF))?;
        if open.buffer.is_none() {
            open.buffer = Some(bytes);
        }
        Ok(())
    }

    /// Puts a handle's buffer if it changed. The put is conditional on the
    /// version the handle last saw, so an edit made elsewhere since then is a
    /// conflict rather than lost.
    async fn flush_handle(&self, fh: u64) -> Result<()> {
        tracing::trace!(target: "scratch::fuse", fh, "flush");
        let (path, data, version, creating) = {
            let handles = self.lock();
            let Some(open) = handles.get(&fh) else {
                return Ok(());
            };
            if !open.dirty {
                return Ok(());
            }
            (
                open.path.clone(),
                open.buffer.clone().unwrap_or_default(),
                open.version.clone(),
                open.creating,
            )
        };
        let (mount, inner) = self.writable("put", &path)?;
        let entry = self
            .call(
                "put",
                &path,
                mount
                    .backend
                    .put(&inner, data, version, creating.unwrap_or(false)),
            )
            .await;
        let mut handles = self.lock();
        if let Some(open) = handles.get_mut(&fh) {
            // A failed put leaves nothing to retry on the next close: the
            // program was told, and retrying a conflict would only repeat it.
            open.dirty = false;
            open.creating = None;
            if let Ok(entry) = &entry {
                open.version = Some(entry.version.clone());
            }
        }
        entry.map(|_| ())
    }

    fn open_handle(&self, open: Open) -> u64 {
        let fh = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.lock().insert(fh, open);
        fh
    }

    async fn truncate(&self, fh: u64, size: u64) -> Result<()> {
        if size > MAX_FILE {
            return Err(errno(FsError::TooLarge.errno()));
        }
        if size > 0 {
            self.load(fh).await?;
        }
        let mut handles = self.lock();
        let held = handles
            .get(&fh)
            .ok_or_else(|| errno(libc::EBADF))?
            .buffer
            .as_ref()
            .map_or(0, |buffer| buffer.len() as u64);
        if size > held {
            Self::room(&handles, size - held)?;
        }
        let open = handles.get_mut(&fh).ok_or_else(|| errno(libc::EBADF))?;
        let buffer = open.buffer.get_or_insert_with(Vec::new);
        buffer.resize(size as usize, 0);
        open.dirty = true;
        Ok(())
    }
}

/// The scratch paths of a rename, which must stay inside one mount.
fn same_mount(from: &str, to: &str) -> Result<()> {
    let (
        Some(MntPath::Inside { plugin, name, .. }),
        Some(MntPath::Inside {
            plugin: to_plugin,
            name: to_name,
            ..
        }),
    ) = (classify(from), classify(to))
    else {
        return Err(errno(libc::EXDEV));
    };
    if plugin != to_plugin || name != to_name {
        return Err(errno(libc::EXDEV));
    }
    Ok(())
}

impl Filesystem for MntFs {
    async fn stat(&self, _cx: &Context, req: pb::Stat) -> Result<pb::FileAttr> {
        tracing::trace!(target: "scratch::fuse", path = %req.path, "stat");
        self.attr(&scratch_path(&req.path)?).await
    }

    async fn read_dir(&self, _cx: &Context, req: pb::ReadDir) -> Result<Vec<pb::DirEntry>> {
        self.list(&scratch_path(&req.path)?).await
    }

    async fn open(&self, _cx: &Context, req: pb::Open) -> Result<pb::OpenReply> {
        tracing::trace!(target: "scratch::fuse", path = %req.path, flags = req.flags, "open");
        let path = scratch_path(&req.path)?;
        let flags = req.flags as i32;
        let writing = flags & libc::O_ACCMODE != libc::O_RDONLY;
        let mut open = Open {
            path: path.clone(),
            buffer: None,
            dirty: false,
            version: None,
            append: false,
            creating: None,
        };
        if classify(&path) == Some(MntPath::Errors) {
            if writing {
                return Err(errno(libc::EACCES));
            }
        } else if !writing {
            let (mount, inner) = self.inside(&path)?;
            let entry = self.call("stat", &path, mount.backend.stat(&inner)).await?;
            if entry.kind == Kind::Dir {
                return Err(errno(libc::EISDIR));
            }
            open.version = Some(entry.version);
        } else {
            let (mount, inner) = self.writable("put", &path)?;
            let entry = self.call("stat", &path, mount.backend.stat(&inner)).await?;
            if entry.kind == Kind::Dir {
                return Err(errno(libc::EISDIR));
            }
            let truncate = flags & libc::O_TRUNC != 0;
            open.buffer = truncate.then(Vec::new);
            open.dirty = truncate;
            open.version = Some(entry.version);
            open.append = flags & libc::O_APPEND != 0;
        }
        let handle = self.open_handle(open);
        Ok(pb::OpenReply {
            handle,
            direct_io: true,
            ..Default::default()
        })
    }

    async fn create(&self, _cx: &Context, req: pb::Create) -> Result<pb::OpenReply> {
        tracing::trace!(target: "scratch::fuse", path = %req.path, flags = req.flags, "create");
        let path = scratch_path(&req.path)?;
        let (mount, inner) = self.writable("put", &path)?;
        let exclusive = req.flags as i32 & libc::O_EXCL != 0;
        if exclusive {
            match mount.backend.stat(&inner).await {
                Ok(_) => return Err(errno(libc::EEXIST)),
                Err(FsError::NotFound) => {}
                Err(error) => {
                    self.errors.record("stat", &path, error);
                    return Err(errno(error.errno()));
                }
            }
        }
        // Put once, with its contents, when it is closed. Until then the
        // handle stands in for it, so a `stat` before the first `close` finds
        // it as it would on a disk.
        let handle = self.open_handle(Open {
            path,
            buffer: Some(Vec::new()),
            dirty: true,
            version: None,
            append: req.flags as i32 & libc::O_APPEND != 0,
            creating: Some(exclusive),
        });
        Ok(pb::OpenReply {
            handle,
            direct_io: true,
            attr: Some(attr_of(Kind::File, 0, now(), mount.read_only)),
            ..Default::default()
        })
    }

    async fn read(&self, _cx: &Context, req: pb::Read) -> Result<Vec<u8>> {
        let (path, buffered) = {
            let handles = self.lock();
            let open = handles.get(&req.handle).ok_or_else(|| errno(libc::EBADF))?;
            (open.path.clone(), open.buffer.clone())
        };
        let slice = |bytes: &[u8]| {
            let start = (req.offset as usize).min(bytes.len());
            let end = (start + req.size as usize).min(bytes.len());
            bytes[start..end].to_vec()
        };
        if classify(&path) == Some(MntPath::Errors) {
            return Ok(slice(&self.errors.render()));
        }
        if let Some(buffer) = buffered {
            return Ok(slice(&buffer));
        }
        let (mount, inner) = self.inside(&path)?;
        self.call(
            "read",
            &path,
            mount.backend.read(&inner, req.offset, req.size),
        )
        .await
    }

    async fn write(&self, _cx: &Context, req: pb::Write) -> Result<pb::WriteReply> {
        tracing::trace!(target: "scratch::fuse", fh = req.handle, offset = req.offset, len = req.data.len(), "write");
        self.load(req.handle).await?;
        let (size, read_only) = {
            let mut handles = self.lock();
            let open = handles.get(&req.handle).ok_or_else(|| errno(libc::EBADF))?;
            let held = open.buffer.as_ref().map_or(0, Vec::len);
            let start = if open.append {
                held
            } else {
                req.offset as usize
            };
            let end = start as u64 + req.data.len() as u64;
            if end > MAX_FILE {
                return Err(errno(FsError::TooLarge.errno()));
            }
            let end = end as usize;
            if held < end {
                Self::room(&handles, (end - held) as u64)?;
            }
            let open = handles
                .get_mut(&req.handle)
                .ok_or_else(|| errno(libc::EBADF))?;
            let buffer = open.buffer.get_or_insert_with(Vec::new);
            if buffer.len() < end {
                buffer.resize(end, 0);
            }
            buffer[start..end].copy_from_slice(&req.data);
            open.dirty = true;
            (buffer.len() as u64, false)
        };
        Ok(pb::WriteReply {
            written: req.data.len() as u32,
            attr: Some(attr_of(Kind::File, size, now(), read_only)),
        })
    }

    async fn flush(&self, _cx: &Context, req: pb::Flush) -> Result<()> {
        self.flush_handle(req.handle).await
    }

    async fn fsync(&self, _cx: &Context, req: pb::Fsync) -> Result<()> {
        self.flush_handle(req.handle).await
    }

    async fn release(&self, _cx: &Context, req: pb::Release) -> Result<()> {
        let flushed = self.flush_handle(req.handle).await;
        self.lock().remove(&req.handle);
        flushed
    }

    async fn set_attr(&self, _cx: &Context, req: pb::SetAttr) -> Result<pb::FileAttr> {
        tracing::trace!(target: "scratch::fuse", path = %req.path, size = ?req.size, fh = ?req.handle, "setattr");
        let path = scratch_path(&req.path)?;
        // Modes, owners and times have nowhere to go in a provider; they are
        // accepted and dropped, as `chmod` on a mount is virtually.
        let Some(size) = req.size else {
            return self.attr(&path).await;
        };
        self.writable("put", &path)?;
        // The handle the truncation came through, or else one this session
        // already has open on the file: a truncation of an open file belongs
        // to its next put, or the two puts would conflict with each other.
        let open = {
            let handles = self.lock();
            match req.handle {
                Some(fh) if handles.contains_key(&fh) => Some(fh),
                _ => handles
                    .iter()
                    .filter(|(_, open)| open.path == path)
                    .map(|(fh, _)| *fh)
                    .max(),
            }
        };
        match open {
            Some(fh) => self.truncate(fh, size).await?,
            None => {
                let (mount, inner) = self.inside(&path)?;
                let entry = self.call("stat", &path, mount.backend.stat(&inner)).await?;
                if entry.size != size {
                    let fh = self.open_handle(Open {
                        path: path.clone(),
                        buffer: None,
                        dirty: false,
                        version: Some(entry.version),
                        append: false,
                        creating: None,
                    });
                    let result = match self.truncate(fh, size).await {
                        Ok(()) => self.flush_handle(fh).await,
                        Err(error) => Err(error),
                    };
                    self.lock().remove(&fh);
                    result?;
                }
            }
        }
        self.attr(&path).await
    }

    async fn mkdir(&self, _cx: &Context, req: pb::Mkdir) -> Result<pb::FileAttr> {
        let path = scratch_path(&req.path)?;
        let (mount, inner) = self.writable("mkdir", &path)?;
        let entry = self
            .call("mkdir", &path, mount.backend.mkdir(&inner))
            .await?;
        Ok(attr_of(
            entry.kind,
            entry.size,
            entry.mtime,
            mount.read_only,
        ))
    }

    async fn unlink(&self, _cx: &Context, req: pb::Unlink) -> Result<()> {
        let path = scratch_path(&req.path)?;
        let (mount, inner) = self.writable("remove", &path)?;
        let entry = self.call("stat", &path, mount.backend.stat(&inner)).await?;
        if entry.kind == Kind::Dir {
            return Err(errno(libc::EISDIR));
        }
        self.call("remove", &path, mount.backend.remove(&inner, false))
            .await
    }

    async fn rmdir(&self, _cx: &Context, req: pb::Rmdir) -> Result<()> {
        let path = scratch_path(&req.path)?;
        let (mount, inner) = self.writable("remove", &path)?;
        if inner == "/" {
            return Err(errno(libc::EBUSY));
        }
        let entry = self.call("stat", &path, mount.backend.stat(&inner)).await?;
        if entry.kind != Kind::Dir {
            return Err(errno(libc::ENOTDIR));
        }
        self.call("remove", &path, mount.backend.remove(&inner, false))
            .await
    }

    async fn rename(&self, _cx: &Context, req: pb::Rename) -> Result<()> {
        let from = scratch_path(&req.from)?;
        let to = scratch_path(&req.to)?;
        same_mount(&from, &to)?;
        let (mount, src) = self.writable("rename", &from)?;
        let (_, dst) = self.inside(&to)?;
        if src == "/" {
            return Err(errno(libc::EBUSY));
        }
        self.call("rename", &from, mount.backend.rename(&src, &dst))
            .await?;
        // Open handles follow the file, so their puts land where it went.
        for open in self.lock().values_mut() {
            if is_at_or_under(&open.path, &from) {
                open.path = format!("{to}{}", &open.path[from.len()..]);
            }
        }
        Ok(())
    }

    async fn stat_fs(&self, _cx: &Context, _req: pb::StatFs) -> Result<pb::StatFsReply> {
        // Providers report no capacity; say there is room so `df` and tools
        // that check free space before writing go ahead.
        Ok(pb::StatFsReply {
            blocks: 1 << 30,
            bfree: 1 << 30,
            bavail: 1 << 30,
            files: 1 << 20,
            ffree: 1 << 20,
            bsize: 4096,
            namelen: 255,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_paths_are_under_mnt() {
        assert_eq!(scratch_path("/").unwrap(), "/mnt");
        assert_eq!(scratch_path("/notes/a").unwrap(), "/mnt/notes/a");
        assert!(scratch_path("relative").is_err());
        assert!(scratch_path("/a/../b").is_err());
    }
}
