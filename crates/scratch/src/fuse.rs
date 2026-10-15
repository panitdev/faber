//! `/mnt` under compute: one FUSE connection serving every mount.
//!
//! gVisor implements `/dev/fuse` inside the sandbox and has no way to take a
//! FUSE connection from outside it, so the connection is made from the
//! inside: `scratch-helper relay` mounts FUSE at `/mnt` and copies messages
//! unchanged over a `SOCK_SEQPACKET` socket to this server, which runs in the
//! Faber process. The socket keeps message boundaries, so to `fuser` the
//! socket is indistinguishable from `/dev/fuse` itself.
//!
//! The server dispatches by path through the session's [`MountTable`] on
//! every request, with zero attribute and entry TTLs, so a mount added or
//! removed while the sandbox runs is visible on the next lookup without
//! touching the connection.
//!
//! Writes are buffered per open file and sent to the provider as one whole-
//! file `put` on `flush` (every `close`), conditional on the version the file
//! had when it was opened. A refused `put` — a conflict above all — reaches
//! the program as the errno of its `close`, and the real error is logged to
//! [`ERRORS_PATH`](crate::mount::ERRORS_PATH), which this server also serves.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenAccMode, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow, WriteFlags,
};

use crate::mount::{
    ERRORS_NAME, ErrorLog, FsError, Kind, MAX_FILE, MNT, MntPath, Mount, MountTable, classify,
    read_all,
};

/// Nothing is cached in the sandbox: the table and the providers are the
/// truth, and a mount added a moment ago must resolve.
const TTL: Duration = Duration::ZERO;

const ROOT: u64 = 1;

/// The largest write the kernel side may send in one request. The relay's
/// buffer must hold a whole request, and FUSE refuses a read into a buffer
/// smaller than this plus its headers — so the two are set together, here and
/// in `scratch-helper`.
pub const MAX_WRITE: u32 = 1 << 20;

/// What all of a session's open handles may buffer together. One sandbox
/// cannot hold more of the Faber process's memory than this, however many
/// files it opens.
const MAX_BUFFERED: u64 = 1 << 30;

/// How many paths the inode table holds. A sandbox can name paths without
/// end; past this, new ones are refused (`ENFILE`) rather than kept.
const MAX_INODES: usize = 1 << 20;

/// An open file and what has been written to it.
struct Open {
    path: String,
    /// The whole file as this handle will put it, once anything was written.
    buffer: Option<Vec<u8>>,
    dirty: bool,
    /// The version the file had when it was opened or last put.
    version: Option<String>,
    /// Opened with `O_APPEND`: every write goes to the end, whatever offset
    /// the request carries (gVisor sends the handle's own offset).
    append: bool,
    /// Made by `create` and not yet put: `Some(exclusive)`. The file is put
    /// once, with its contents, when the handle is flushed.
    creating: Option<bool>,
}

/// The FUSE server for one session's `/mnt`.
pub struct MntFs {
    mounts: Arc<MountTable>,
    errors: Arc<ErrorLog>,
    runtime: tokio::runtime::Handle,
    inodes: Mutex<Inodes>,
    handles: Mutex<HashMap<u64, Open>>,
    next_handle: AtomicU64,
}

/// Paths and inode numbers, both ways. Never shrinks, since reusing a number
/// risks handing a stale one to the wrong file; a path gets a number only
/// once it was found, and the table stops at [`MAX_INODES`].
struct Inodes {
    by_ino: HashMap<u64, String>,
    by_path: HashMap<String, u64>,
    next: u64,
}

impl Inodes {
    fn new() -> Self {
        let mut inodes = Inodes {
            by_ino: HashMap::new(),
            by_path: HashMap::new(),
            next: ROOT + 1,
        };
        inodes.by_ino.insert(ROOT, MNT.to_owned());
        inodes.by_path.insert(MNT.to_owned(), ROOT);
        inodes
    }

    fn ino(&mut self, path: &str) -> Result<u64, Errno> {
        if let Some(ino) = self.by_path.get(path) {
            return Ok(*ino);
        }
        if self.by_path.len() >= MAX_INODES {
            return Err(Errno::ENFILE);
        }
        let ino = self.next;
        self.next += 1;
        self.by_ino.insert(ino, path.to_owned());
        self.by_path.insert(path.to_owned(), ino);
        Ok(ino)
    }

    /// Moves `from` and everything under it to `to`, keeping their numbers.
    fn rename(&mut self, from: &str, to: &str) {
        let moved: Vec<(String, u64)> = self
            .by_path
            .iter()
            .filter(|(path, _)| crate::layer::is_at_or_under(path, from))
            .map(|(path, ino)| (path.clone(), *ino))
            .collect();
        for (old, ino) in moved {
            let new = format!("{to}{}", &old[from.len()..]);
            self.by_path.remove(&old);
            self.by_path.insert(new.clone(), ino);
            self.by_ino.insert(ino, new);
        }
    }
}

fn errno(code: i32) -> Errno {
    Errno::from_i32(code)
}

impl MntFs {
    pub fn new(
        mounts: Arc<MountTable>,
        errors: Arc<ErrorLog>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        MntFs {
            mounts,
            errors,
            runtime,
            inodes: Mutex::new(Inodes::new()),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        }
    }

    /// Serves the connection on `socket` on a thread of its own until the
    /// relay closes it. Returns once FUSE's `INIT` has been answered.
    pub fn serve(self, socket: OwnedFd) -> std::io::Result<fuser::BackgroundSession> {
        let mut config = fuser::Config::default();
        // Requests arrive as the sandbox's uids, never the Faber process's,
        // and the sandbox decides who may touch `/mnt`.
        config.acl = fuser::SessionACL::All;
        config.n_threads = Some(1);
        fuser::Session::from_fd(self, socket, fuser::SessionACL::All, config)?.spawn()
    }

    fn path(&self, ino: INodeNo) -> Result<String, Errno> {
        self.inodes
            .lock()
            .expect("inodes poisoned")
            .by_ino
            .get(&ino.0)
            .cloned()
            .ok_or(Errno::ENOENT)
    }

    fn ino(&self, path: &str) -> Result<u64, Errno> {
        self.inodes.lock().expect("inodes poisoned").ino(path)
    }

    fn child(&self, parent: INodeNo, name: &OsStr) -> Result<String, Errno> {
        let parent = self.path(parent)?;
        let name = name.to_str().ok_or(Errno::EINVAL)?;
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(Errno::EINVAL);
        }
        Ok(format!("{parent}/{name}"))
    }

    fn mount(&self, plugin: &str, name: &str) -> Result<Mount, Errno> {
        self.mounts.get(plugin, name).ok_or(Errno::ENOENT)
    }

    /// The mount and mount-relative path of a scratch path inside a mount.
    fn inside(&self, path: &str) -> Result<(Mount, String), Errno> {
        match classify(path) {
            Some(MntPath::Inside { plugin, name, path }) => Ok((self.mount(&plugin, &name)?, path)),
            Some(MntPath::Errors) => Err(Errno::EACCES),
            _ => Err(Errno::EISDIR),
        }
    }

    /// Like [`inside`](Self::inside), refusing a read-only mount the way
    /// virtual mode does: before the provider is asked, and logged.
    fn writable(&self, op: &str, path: &str) -> Result<(Mount, String), Errno> {
        let (mount, inner) = match classify(path) {
            Some(MntPath::Inside { .. }) => self.inside(path)?,
            // `/mnt` and `/mnt/<plugin>` belong to the table, not a provider.
            _ => return Err(Errno::EACCES),
        };
        if mount.read_only {
            self.errors.record(op, path, FsError::ReadOnly);
            return Err(errno(FsError::ReadOnly.errno()));
        }
        Ok((mount, inner))
    }

    /// Runs a provider call, logging any error but a plain miss.
    fn call<T>(
        &self,
        op: &str,
        path: &str,
        future: impl std::future::Future<Output = Result<T, FsError>>,
    ) -> Result<T, Errno> {
        self.runtime.block_on(future).map_err(|error| {
            if error != FsError::NotFound {
                self.errors.record(op, path, error);
            }
            errno(error.errno())
        })
    }

    fn attr_of(&self, ino: u64, kind: Kind, size: u64, mtime: u64, read_only: bool) -> FileAttr {
        let time = UNIX_EPOCH + Duration::from_secs(mtime);
        let (kind, perm, nlink) = match kind {
            Kind::Dir => (FileType::Directory, 0o755, 2),
            Kind::File if read_only => (FileType::RegularFile, 0o444, 1),
            Kind::File => (FileType::RegularFile, 0o644, 1),
        };
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: size.div_ceil(512),
            atime: time,
            mtime: time,
            ctime: time,
            crtime: time,
            kind,
            perm,
            nlink,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    /// The size an open, written handle gives a path, which the provider does
    /// not know yet.
    fn buffered_size(&self, path: &str) -> Option<u64> {
        let handles = self.handles.lock().expect("handles poisoned");
        handles
            .values()
            .filter(|open| open.path == path && open.dirty)
            .find_map(|open| open.buffer.as_ref().map(|buffer| buffer.len() as u64))
    }

    /// A path's attributes. Its inode number is handed out only once the
    /// path is known to exist, so naming paths that do not costs nothing.
    fn attr(&self, path: &str) -> Result<FileAttr, Errno> {
        match classify(path).ok_or(Errno::ENOENT)? {
            MntPath::Root => Ok(self.attr_of(self.ino(path)?, Kind::Dir, 0, 0, false)),
            MntPath::Errors => {
                let size = self.errors.render().len() as u64;
                Ok(self.attr_of(self.ino(path)?, Kind::File, size, now(), true))
            }
            MntPath::Plugin(plugin) => {
                if self.mounts.plugins().contains(&plugin) {
                    Ok(self.attr_of(self.ino(path)?, Kind::Dir, 0, 0, false))
                } else {
                    Err(Errno::ENOENT)
                }
            }
            MntPath::Inside {
                plugin,
                name,
                path: inner,
            } => {
                let mount = self.mount(&plugin, &name)?;
                if let Some(size) = self.creating_size(path) {
                    return Ok(self.attr_of(
                        self.ino(path)?,
                        Kind::File,
                        size,
                        now(),
                        mount.read_only,
                    ));
                }
                let entry = self.call("stat", path, mount.backend.stat(&inner))?;
                let size = self.buffered_size(path).unwrap_or(entry.size);
                Ok(self.attr_of(
                    self.ino(path)?,
                    entry.kind,
                    size,
                    entry.mtime,
                    mount.read_only,
                ))
            }
        }
    }

    /// The size of a file this session created and has not put yet, which
    /// the provider does not know of.
    fn creating_size(&self, path: &str) -> Option<u64> {
        let handles = self.handles.lock().expect("handles poisoned");
        handles
            .values()
            .find(|open| open.path == path && open.creating.is_some())
            .map(|open| open.buffer.as_ref().map_or(0, |buffer| buffer.len() as u64))
    }

    /// Whether the handles may grow by `more` bytes, all together.
    fn room(handles: &HashMap<u64, Open>, more: u64) -> Result<(), Errno> {
        let held: u64 = handles
            .values()
            .map(|open| open.buffer.as_ref().map_or(0, |buffer| buffer.len() as u64))
            .sum();
        if held.saturating_add(more) > MAX_BUFFERED {
            return Err(Errno::ENOSPC);
        }
        Ok(())
    }

    /// Children of a directory as `(name, kind)`.
    fn list(&self, path: &str) -> Result<Vec<(String, FileType)>, Errno> {
        match classify(path).ok_or(Errno::ENOENT)? {
            MntPath::Root => {
                let mut names: Vec<(String, FileType)> = self
                    .mounts
                    .plugins()
                    .into_iter()
                    .map(|plugin| (plugin, FileType::Directory))
                    .collect();
                names.push((ERRORS_NAME.to_owned(), FileType::RegularFile));
                Ok(names)
            }
            MntPath::Plugin(plugin) => {
                let names = self.mounts.names(&plugin);
                if names.is_empty() {
                    return Err(Errno::ENOENT);
                }
                Ok(names
                    .into_iter()
                    .map(|name| (name, FileType::Directory))
                    .collect())
            }
            MntPath::Errors => Err(Errno::ENOTDIR),
            MntPath::Inside {
                plugin,
                name,
                path: inner,
            } => {
                let mount = self.mount(&plugin, &name)?;
                let entries = self.call("read-dir", path, mount.backend.read_dir(&inner))?;
                Ok(entries
                    .into_iter()
                    .map(|entry| {
                        let kind = match entry.kind {
                            Kind::Dir => FileType::Directory,
                            Kind::File => FileType::RegularFile,
                        };
                        (entry.name, kind)
                    })
                    .collect())
            }
        }
    }

    /// Makes sure a handle holds the whole file, reading it on first need.
    fn load(&self, fh: u64) -> Result<(), Errno> {
        let path = {
            let handles = self.handles.lock().expect("handles poisoned");
            let open = handles.get(&fh).ok_or(Errno::EBADF)?;
            if open.buffer.is_some() {
                return Ok(());
            }
            open.path.clone()
        };
        let (mount, inner) = self.inside(&path)?;
        let bytes = self.call("read", &path, read_all(mount.backend.as_ref(), &inner))?;
        tracing::trace!(target: "scratch::fuse", fh, loaded = bytes.len(), "load");
        let mut handles = self.handles.lock().expect("handles poisoned");
        Self::room(&handles, bytes.len() as u64)?;
        let open = handles.get_mut(&fh).ok_or(Errno::EBADF)?;
        if open.buffer.is_none() {
            open.buffer = Some(bytes);
        }
        Ok(())
    }

    /// Puts a handle's buffer if it changed. The put is conditional on the
    /// version the handle last saw, so an edit made elsewhere since then is a
    /// conflict rather than lost.
    fn flush_handle(&self, fh: u64) -> Result<(), Errno> {
        tracing::trace!(target: "scratch::fuse", fh, "flush");
        let (path, data, version, creating) = {
            let handles = self.handles.lock().expect("handles poisoned");
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
        let entry = self.call(
            "put",
            &path,
            mount
                .backend
                .put(&inner, data, version, creating.unwrap_or(false)),
        );
        let mut handles = self.handles.lock().expect("handles poisoned");
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

    fn open_handle(
        &self,
        path: String,
        buffer: Option<Vec<u8>>,
        dirty: bool,
        version: Option<String>,
    ) -> u64 {
        let fh = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().expect("handles poisoned").insert(
            fh,
            Open {
                path,
                buffer,
                dirty,
                version,
                append: false,
                creating: None,
            },
        );
        fh
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl Filesystem for MntFs {
    fn init(&mut self, _req: &Request, config: &mut fuser::KernelConfig) -> std::io::Result<()> {
        let _ = config.set_max_write(MAX_WRITE);
        Ok(())
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        tracing::trace!(target: "scratch::fuse", parent = parent.0, ?name, "lookup");
        match self.child(parent, name).and_then(|path| self.attr(&path)) {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    /// Existence, and writability where a mount is read-only; the sandbox
    /// runs as root, so there are no other permissions to check.
    fn access(&self, _req: &Request, ino: INodeNo, mask: fuser::AccessFlags, reply: ReplyEmpty) {
        let result = self.path(ino).and_then(|path| {
            let attr = self.attr(&path)?;
            if mask.contains(fuser::AccessFlags::W_OK) && attr.perm & 0o200 == 0 {
                return Err(errno(libc::EROFS));
            }
            Ok(())
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        tracing::trace!(target: "scratch::fuse", ino = ino.0, "getattr");
        match self.path(ino).and_then(|path| self.attr(&path)) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(error),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        tracing::trace!(target: "scratch::fuse", ino = ino.0, ?size, ?fh, "setattr");
        let result = (|| {
            let path = self.path(ino)?;
            // Modes, owners and times have nowhere to go in a provider; they
            // are accepted and dropped, as `chmod` on a mount is virtually.
            if let Some(size) = size {
                self.writable("put", &path)?;
                // The handle the truncation came through, or else one this
                // session already has open on the file: a truncation of an
                // open file belongs to its next put, or the two puts would
                // conflict with each other.
                let open = {
                    let handles = self.handles.lock().expect("handles poisoned");
                    match fh {
                        Some(fh) if handles.contains_key(&fh.0) => Some(fh.0),
                        _ => handles
                            .iter()
                            .filter(|(_, open)| open.path == path)
                            .map(|(fh, _)| *fh)
                            .max(),
                    }
                };
                let fh = match open {
                    Some(fh) => fh,
                    None => {
                        let (mount, inner) = self.inside(&path)?;
                        let entry = self.call("stat", &path, mount.backend.stat(&inner))?;
                        if entry.size == size {
                            return self.attr(&path);
                        }
                        let fh = self.open_handle(path.clone(), None, false, Some(entry.version));
                        let result = self.truncate(fh, size).and_then(|()| self.flush_handle(fh));
                        self.handles.lock().expect("handles poisoned").remove(&fh);
                        result?;
                        return self.attr(&path);
                    }
                };
                self.truncate(fh, size)?;
            }
            self.attr(&path)
        })();
        match result {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(error) => reply.error(error),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child(parent, name)?;
            let (mount, inner) = self.writable("mkdir", &path)?;
            let entry = self.call("mkdir", &path, mount.backend.mkdir(&inner))?;
            Ok(self.attr_of(
                self.ino(&path)?,
                entry.kind,
                entry.size,
                entry.mtime,
                mount.read_only,
            ))
        })();
        match result {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = (|| {
            let path = self.child(parent, name)?;
            let (mount, inner) = self.writable("remove", &path)?;
            let entry = self.call("stat", &path, mount.backend.stat(&inner))?;
            if entry.kind == Kind::Dir {
                return Err(Errno::EISDIR);
            }
            self.call("remove", &path, mount.backend.remove(&inner, false))
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = (|| {
            let path = self.child(parent, name)?;
            let (mount, inner) = self.writable("remove", &path)?;
            if inner == "/" {
                return Err(Errno::EBUSY);
            }
            let entry = self.call("stat", &path, mount.backend.stat(&inner))?;
            if entry.kind != Kind::Dir {
                return Err(Errno::ENOTDIR);
            }
            self.call("remove", &path, mount.backend.remove(&inner, false))
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let result = (|| {
            let from = self.child(parent, name)?;
            let to = self.child(newparent, newname)?;
            let (
                Some(MntPath::Inside { plugin, name, .. }),
                Some(MntPath::Inside {
                    plugin: to_plugin,
                    name: to_name,
                    ..
                }),
            ) = (classify(&from), classify(&to))
            else {
                return Err(Errno::EXDEV);
            };
            if plugin != to_plugin || name != to_name {
                return Err(Errno::EXDEV);
            }
            let (mount, src) = self.writable("rename", &from)?;
            let (_, dst) = self.inside(&to)?;
            if src == "/" {
                return Err(Errno::EBUSY);
            }
            self.call("rename", &from, mount.backend.rename(&src, &dst))?;
            self.inodes
                .lock()
                .expect("inodes poisoned")
                .rename(&from, &to);
            Ok(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        tracing::trace!(target: "scratch::fuse", ino = ino.0, flags = flags.0, "open");
        let result = (|| {
            let path = self.path(ino)?;
            let writing = flags.acc_mode() != OpenAccMode::O_RDONLY;
            if classify(&path) == Some(MntPath::Errors) {
                if writing {
                    return Err(Errno::EACCES);
                }
                return Ok(self.open_handle(path, None, false, None));
            }
            if !writing {
                let (mount, inner) = self.inside(&path)?;
                let entry = self.call("stat", &path, mount.backend.stat(&inner))?;
                if entry.kind == Kind::Dir {
                    return Err(Errno::EISDIR);
                }
                return Ok(self.open_handle(path, None, false, Some(entry.version)));
            }
            let (mount, inner) = self.writable("put", &path)?;
            let entry = self.call("stat", &path, mount.backend.stat(&inner))?;
            if entry.kind == Kind::Dir {
                return Err(Errno::EISDIR);
            }
            let truncate = flags.0 & libc::O_TRUNC != 0;
            let fh = self.open_handle(path, truncate.then(Vec::new), truncate, Some(entry.version));
            if flags.0 & libc::O_APPEND != 0
                && let Some(open) = self.handles.lock().expect("handles poisoned").get_mut(&fh)
            {
                open.append = true;
            }
            Ok(fh)
        })();
        match result {
            Ok(fh) => reply.opened(FileHandle(fh), FopenFlags::FOPEN_DIRECT_IO),
            Err(error) => reply.error(error),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        tracing::trace!(target: "scratch::fuse", parent = parent.0, ?name, flags, "create");
        let result = (|| {
            let path = self.child(parent, name)?;
            let (mount, inner) = self.writable("put", &path)?;
            let exclusive = flags & libc::O_EXCL != 0;
            if exclusive {
                match self.runtime.block_on(mount.backend.stat(&inner)) {
                    Ok(_) => return Err(Errno::EEXIST),
                    Err(FsError::NotFound) => {}
                    Err(error) => {
                        self.errors.record("stat", &path, error);
                        return Err(errno(error.errno()));
                    }
                }
            }
            // Put once, with its contents, when it is closed. Until then the
            // handle stands in for it, so a `stat` before the first `close`
            // finds it as it would on a disk.
            let ino = self.ino(&path)?;
            let fh = self.open_handle(path.clone(), Some(Vec::new()), true, None);
            if let Some(open) = self.handles.lock().expect("handles poisoned").get_mut(&fh) {
                open.creating = Some(exclusive);
            }
            Ok((self.attr_of(ino, Kind::File, 0, now(), mount.read_only), fh))
        })();
        match result {
            Ok((attr, fh)) => reply.created(
                &TTL,
                &attr,
                Generation(0),
                FileHandle(fh),
                FopenFlags::FOPEN_DIRECT_IO,
            ),
            Err(error) => reply.error(error),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let result = (|| {
            let (path, buffered) = {
                let handles = self.handles.lock().expect("handles poisoned");
                let open = handles.get(&fh.0).ok_or(Errno::EBADF)?;
                (open.path.clone(), open.buffer.clone())
            };
            let slice = |bytes: &[u8]| {
                let start = (offset as usize).min(bytes.len());
                let end = (start + size as usize).min(bytes.len());
                bytes[start..end].to_vec()
            };
            if classify(&path) == Some(MntPath::Errors) {
                return Ok(slice(&self.errors.render()));
            }
            if let Some(buffer) = buffered {
                return Ok(slice(&buffer));
            }
            let (mount, inner) = self.inside(&path)?;
            self.call("read", &path, mount.backend.read(&inner, offset, size))
        })();
        match result {
            Ok(bytes) => reply.data(&bytes),
            Err(error) => reply.error(error),
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        tracing::trace!(target: "scratch::fuse", fh = fh.0, offset, len = data.len(), flags = flags.0, "write");
        let result = (|| {
            self.load(fh.0)?;
            let mut handles = self.handles.lock().expect("handles poisoned");
            let open = handles.get_mut(&fh.0).ok_or(Errno::EBADF)?;
            let buffer = open.buffer.get_or_insert_with(Vec::new);
            let start = if open.append || flags.0 & libc::O_APPEND != 0 {
                buffer.len()
            } else {
                offset as usize
            };
            let end = start as u64 + data.len() as u64;
            if end > MAX_FILE {
                return Err(errno(FsError::TooLarge.errno()));
            }
            let end = end as usize;
            if buffer.len() < end {
                let more = (end - buffer.len()) as u64;
                Self::room(&handles, more)?;
                let open = handles.get_mut(&fh.0).ok_or(Errno::EBADF)?;
                let buffer = open.buffer.get_or_insert_with(Vec::new);
                buffer.resize(end, 0);
                buffer[start..end].copy_from_slice(data);
                open.dirty = true;
                return Ok(data.len() as u32);
            }
            buffer[start..end].copy_from_slice(data);
            open.dirty = true;
            Ok(data.len() as u32)
        })();
        match result {
            Ok(written) => reply.written(written),
            Err(error) => reply.error(error),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        match self.flush_handle(fh.0) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.flush_handle(fh.0) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let flushed = self.flush_handle(fh.0);
        self.handles.lock().expect("handles poisoned").remove(&fh.0);
        match flushed {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let result = self
            .path(ino)
            .and_then(|path| Ok((self.list(&path)?, path)));
        let (children, path) = match result {
            Ok(found) => found,
            Err(error) => return reply.error(error),
        };
        let parent = match path.rfind('/') {
            Some(at) if at > 0 && path != MNT => match self.ino(&path[..at]) {
                Ok(parent) => parent,
                Err(error) => return reply.error(error),
            },
            _ => ROOT,
        };
        let mut all = vec![
            (ino.0, FileType::Directory, ".".to_owned()),
            (parent, FileType::Directory, "..".to_owned()),
        ];
        for (name, kind) in children {
            match self.ino(&format!("{path}/{name}")) {
                Ok(child) => all.push((child, kind, name)),
                Err(error) => return reply.error(error),
            }
        }
        for (index, (child, kind, name)) in all.into_iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(child), (index + 1) as u64, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}

impl MntFs {
    fn truncate(&self, fh: u64, size: u64) -> Result<(), Errno> {
        if size > MAX_FILE {
            return Err(errno(FsError::TooLarge.errno()));
        }
        if size > 0 {
            self.load(fh)?;
        }
        let mut handles = self.handles.lock().expect("handles poisoned");
        let held = handles
            .get(&fh)
            .ok_or(Errno::EBADF)?
            .buffer
            .as_ref()
            .map_or(0, |buffer| buffer.len() as u64);
        if size > held {
            Self::room(&handles, size - held)?;
        }
        let open = handles.get_mut(&fh).ok_or(Errno::EBADF)?;
        let buffer = open.buffer.get_or_insert_with(Vec::new);
        buffer.resize(size as usize, 0);
        open.dirty = true;
        Ok(())
    }
}
