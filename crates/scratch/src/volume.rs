//! A project's volume as scratch works in it: a directory tree addressed by
//! absolute paths inside it (`/` is its top), wherever it really is.
//!
//! - [`TenantdVolume`]: a volume in tenantd, reached only through tenantd's
//!   `Files` API, as the volume's owner. Nothing about it has to be on this
//!   host, so tenantd may run anywhere the API can reach.
//! - [`LocalVolume`]: a directory on this host — the projects' volumes when
//!   there is no tenantd, and the operator's toolstore.
//!
//! Every call takes a path scratch has resolved itself: it follows symlinks
//! in its own namespace, one component at a time, because a link inside a
//! volume may point anywhere in scratch, not only inside the volume. So a
//! volume never needs to follow a link, and a path's last component is never
//! one when it reads, writes or changes a file.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tenantd_client::files::{self, Files};
use tenantd_client::pb;
use tonic::transport::Channel;

use crate::beneath::{self, Beneath};
use crate::vfs::errno_error;

/// What a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryType {
    File,
    Dir,
    Symlink,
    Fifo,
    Other,
}

/// What scratch needs to know about a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub kind: EntryType,
    /// Permission, setuid/setgid and sticky bits.
    pub mode: u32,
    pub size: u64,
    pub modified: SystemTime,
}

impl Meta {
    pub fn is_dir(&self) -> bool {
        self.kind == EntryType::Dir
    }

    pub fn is_symlink(&self) -> bool {
        self.kind == EntryType::Symlink
    }

    pub fn from_std(meta: &fs::Metadata) -> Meta {
        use std::os::unix::fs::FileTypeExt;
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            EntryType::Symlink
        } else if file_type.is_dir() {
            EntryType::Dir
        } else if file_type.is_file() {
            EntryType::File
        } else if file_type.is_fifo() {
            EntryType::Fifo
        } else {
            EntryType::Other
        };
        Meta {
            kind,
            mode: meta.mode() & 0o7777,
            size: meta.len(),
            modified: meta.modified().unwrap_or(UNIX_EPOCH),
        }
    }

    fn from_attr(attr: &pb::FileAttr) -> Meta {
        let kind = match attr.kind() {
            pb::FileKind::File => EntryType::File,
            pb::FileKind::Directory => EntryType::Dir,
            pb::FileKind::Symlink => EntryType::Symlink,
            pb::FileKind::Fifo => EntryType::Fifo,
            _ => EntryType::Other,
        };
        let modified = if attr.mtime_ns >= 0 {
            UNIX_EPOCH + Duration::from_nanos(attr.mtime_ns as u64)
        } else {
            UNIX_EPOCH
        };
        Meta {
            kind,
            mode: attr.mode & 0o7777,
            size: attr.size,
            modified,
        }
    }
}

/// A volume's files. Paths are absolute inside the volume and already
/// resolved by scratch (see the module docs).
#[async_trait]
pub trait Volume: Send + Sync {
    /// What is at `path`, without following a final symlink; `None` when
    /// nothing is, or a directory on the way is missing or no directory.
    async fn lstat(&self, path: &str) -> io::Result<Option<Meta>>;
    /// The directory's entries, each `lstat`-ed. A missing directory has
    /// none.
    async fn read_dir(&self, path: &str) -> io::Result<Vec<(String, Meta)>>;
    /// A regular file's bytes.
    async fn read(&self, path: &str) -> io::Result<Vec<u8>>;
    /// Replaces (or creates) the file with `data`; `mode` when given, else
    /// the replaced file's, or 0644.
    async fn write(&self, path: &str, data: &[u8], mode: Option<u32>) -> io::Result<()>;
    /// Adds `data` at the end of the file, creating it (0644) if missing.
    async fn append(&self, path: &str, data: &[u8]) -> io::Result<()>;
    /// Makes a directory with `mode` (0755 when `None`). With `parents`,
    /// missing parents are made and an existing directory is no error.
    async fn mkdir(&self, path: &str, mode: Option<u32>, parents: bool) -> io::Result<()>;
    /// Makes a symlink at `path` to `target`, stored as given.
    async fn symlink(&self, target: &str, path: &str) -> io::Result<()>;
    async fn read_link(&self, path: &str) -> io::Result<PathBuf>;
    /// Removes a file, or an empty directory with `dir`.
    async fn remove(&self, path: &str, dir: bool) -> io::Result<()>;
    /// Removes what is at `path` and everything under it.
    async fn remove_all(&self, path: &str) -> io::Result<()> {
        remove_all(self, path).await
    }
    /// Renames within the volume, replacing what is at `to` as `rename(2)`
    /// does.
    async fn rename(&self, from: &str, to: &str) -> io::Result<()>;
    /// Sets the mode and/or the modification time; `None` leaves it.
    async fn set_attr(
        &self,
        path: &str,
        mode: Option<u32>,
        modified: Option<SystemTime>,
    ) -> io::Result<()>;
}

/// [`Volume::remove_all`] in terms of the other calls, depth first.
async fn remove_all<V: Volume + ?Sized>(volume: &V, path: &str) -> io::Result<()> {
    let Some(meta) = volume.lstat(path).await? else {
        return Err(errno_error(libc::ENOENT));
    };
    if !meta.is_dir() {
        return volume.remove(path, false).await;
    }
    let mut pending = vec![(path.to_owned(), false)];
    while let Some((dir, listed)) = pending.pop() {
        if listed {
            volume.remove(&dir, true).await?;
            continue;
        }
        pending.push((dir.clone(), true));
        for (name, meta) in volume.read_dir(&dir).await? {
            let child = if dir == "/" {
                format!("/{name}")
            } else {
                format!("{dir}/{name}")
            };
            if meta.is_dir() {
                pending.push((child, false));
            } else {
                volume.remove(&child, false).await?;
            }
        }
    }
    Ok(())
}

/// A volume in tenantd, through its `Files` API.
pub struct TenantdVolume {
    files: Files,
}

impl TenantdVolume {
    pub fn new(channel: Channel, tenant: &str, id: &str) -> TenantdVolume {
        TenantdVolume {
            files: Files::volume(
                channel,
                pb::VolumeRef {
                    tenant: tenant.to_owned(),
                    id: id.to_owned(),
                },
            ),
        }
    }

    fn files(&self) -> Files {
        self.files.clone()
    }
}

fn tenantd_error(error: files::Error) -> io::Error {
    match error {
        files::Error::Errno { errno, .. } => errno_error(errno),
        other => io::Error::other(other.to_string()),
    }
}

fn gone(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

#[async_trait]
impl Volume for TenantdVolume {
    async fn lstat(&self, path: &str) -> io::Result<Option<Meta>> {
        match self.files().stat(path, false).await {
            Ok(stat) => Ok(Some(Meta::from_attr(&stat.attr))),
            Err(error) => {
                let error = tenantd_error(error);
                if gone(&error) { Ok(None) } else { Err(error) }
            }
        }
    }

    async fn read_dir(&self, path: &str) -> io::Result<Vec<(String, Meta)>> {
        match self.files().read_dir(path).await {
            Ok(entries) => Ok(entries
                .into_iter()
                .map(|entry| {
                    let meta = Meta::from_attr(&entry.attr.unwrap_or_default());
                    (entry.name, meta)
                })
                .collect()),
            Err(error) => {
                let error = tenantd_error(error);
                if gone(&error) {
                    Ok(Vec::new())
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        self.files().read(path).await.map_err(tenantd_error)
    }

    async fn write(&self, path: &str, data: &[u8], mode: Option<u32>) -> io::Result<()> {
        let options = files::WriteOptions {
            mode,
            ..Default::default()
        };
        self.files()
            .write(path, data.to_vec(), &options)
            .await
            .map(|_| ())
            .map_err(tenantd_error)
    }

    async fn append(&self, path: &str, data: &[u8]) -> io::Result<()> {
        // tenantd replaces a file whole; an append is a read and a write.
        let mut body = match self.files().read(path).await.map_err(tenantd_error) {
            Ok(body) => body,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        body.extend_from_slice(data);
        self.write(path, &body, None).await
    }

    async fn mkdir(&self, path: &str, mode: Option<u32>, parents: bool) -> io::Result<()> {
        self.files()
            .mkdir(path, mode, parents)
            .await
            .map(|_| ())
            .map_err(tenantd_error)
    }

    async fn symlink(&self, target: &str, path: &str) -> io::Result<()> {
        self.files()
            .symlink(path, target)
            .await
            .map_err(tenantd_error)
    }

    async fn read_link(&self, path: &str) -> io::Result<PathBuf> {
        let target = self.files().read_link(path).await.map_err(tenantd_error)?;
        Ok(PathBuf::from(String::from_utf8_lossy(&target).into_owned()))
    }

    async fn remove(&self, path: &str, dir: bool) -> io::Result<()> {
        let mut files = self.files();
        if dir {
            files.remove_dir(path).await
        } else {
            files.remove(path).await
        }
        .map_err(tenantd_error)
    }

    async fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.files()
            .rename(from, to, 0)
            .await
            .map_err(tenantd_error)
    }

    async fn set_attr(
        &self,
        path: &str,
        mode: Option<u32>,
        modified: Option<SystemTime>,
    ) -> io::Result<()> {
        let mtime_ns = modified.map(|time| match time.duration_since(UNIX_EPOCH) {
            Ok(since) => since.as_nanos().min(i64::MAX as u128) as i64,
            Err(before) => -(before.duration().as_nanos().min(i64::MAX as u128) as i64),
        });
        self.files()
            .set_attr(path, mode, mtime_ns)
            .await
            .map(|_| ())
            .map_err(tenantd_error)
    }
}

/// A directory on this host. No symlink on the way to a file is ever
/// followed ([`Beneath`]), so nothing leads out of it.
pub struct LocalVolume {
    root: PathBuf,
}

impl LocalVolume {
    pub fn new(root: impl Into<PathBuf>) -> LocalVolume {
        LocalVolume { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn at(&self, path: &str) -> io::Result<Beneath> {
        Beneath::new(&self.root, path)
    }
}

fn lstat(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if gone(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The entries of the directory at `relative` under `root`, without
/// following a symlink anywhere on the way. A missing directory, or a
/// symlink where it should be, has none.
pub(crate) fn read_names(root: &Path, relative: &str) -> io::Result<Vec<(String, Meta)>> {
    let real = match Beneath::new(root, relative) {
        Ok(real) => real,
        Err(error) if gone(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let dir = match beneath::open_dir(&real) {
        Ok(dir) => dir,
        Err(error) if gone(&error) || error.raw_os_error() == Some(libc::ELOOP) => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if gone(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Ok(meta) = fs::symlink_metadata(entry.path()) {
            names.push((name, Meta::from_std(&meta)));
        }
    }
    Ok(names)
}

#[async_trait]
impl Volume for LocalVolume {
    async fn lstat(&self, path: &str) -> io::Result<Option<Meta>> {
        let real = match self.at(path) {
            Ok(real) => real,
            Err(error) if gone(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(lstat(&real)?.map(|meta| Meta::from_std(&meta)))
    }

    async fn read_dir(&self, path: &str) -> io::Result<Vec<(String, Meta)>> {
        read_names(&self.root, path)
    }

    async fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        beneath::open_nofollow(&self.at(path)?)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    async fn write(&self, path: &str, data: &[u8], mode: Option<u32>) -> io::Result<()> {
        let real = self.at(path)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode.unwrap_or(0o644))
            .custom_flags(libc::O_NOFOLLOW)
            .open(&real)?;
        file.write_all(data)?;
        if let Some(mode) = mode {
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        Ok(())
    }

    async fn append(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let real = self.at(path)?;
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&real)?
            .write_all(data)
    }

    async fn mkdir(&self, path: &str, mode: Option<u32>, parents: bool) -> io::Result<()> {
        let mode = mode.unwrap_or(0o755);
        let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        let upto = if parents { 1 } else { parts.len() };
        for end in upto..=parts.len() {
            let here = format!("/{}", parts[..end].join("/"));
            let last = end == parts.len();
            let real = self.at(&here)?;
            match fs::create_dir(&real) {
                Ok(()) => beneath::set_mode(&real, if last { mode } else { 0o755 })?,
                Err(error)
                    if error.kind() == io::ErrorKind::AlreadyExists && (parents || !last) =>
                {
                    if !lstat(&real)?.is_some_and(|meta| meta.is_dir()) {
                        return Err(errno_error(libc::ENOTDIR));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        if parts.is_empty() && !parents {
            return Err(errno_error(libc::EEXIST));
        }
        Ok(())
    }

    async fn symlink(&self, target: &str, path: &str) -> io::Result<()> {
        symlink(target, self.at(path)?)
    }

    async fn read_link(&self, path: &str) -> io::Result<PathBuf> {
        fs::read_link(self.at(path)?)
    }

    async fn remove(&self, path: &str, dir: bool) -> io::Result<()> {
        let real = self.at(path)?;
        if dir {
            fs::remove_dir(&real)
        } else {
            fs::remove_file(&real)
        }
    }

    async fn remove_all(&self, path: &str) -> io::Result<()> {
        let real = self.at(path)?;
        match lstat(&real)? {
            Some(meta) if meta.is_dir() => fs::remove_dir_all(&real),
            Some(_) => fs::remove_file(&real),
            None => Err(errno_error(libc::ENOENT)),
        }
    }

    async fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        fs::rename(self.at(from)?, self.at(to)?)
    }

    async fn set_attr(
        &self,
        path: &str,
        mode: Option<u32>,
        modified: Option<SystemTime>,
    ) -> io::Result<()> {
        let real = self.at(path)?;
        if let Some(mode) = mode {
            beneath::set_mode(&real, mode & 0o7777)?;
        }
        if let Some(time) = modified {
            beneath::open_nofollow(&real)?.set_modified(time)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("faber-volume-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn a_local_volume_works_inside_its_directory() {
        let dir = temp("local");
        let volume = LocalVolume::new(&dir);
        volume.mkdir("/a/b", Some(0o750), true).await.unwrap();
        assert_eq!(volume.lstat("/a/b").await.unwrap().unwrap().mode, 0o750);
        volume.write("/a/b/f", b"one", None).await.unwrap();
        volume.append("/a/b/f", b"two").await.unwrap();
        assert_eq!(volume.read("/a/b/f").await.unwrap(), b"onetwo");
        volume.symlink("/etc", "/a/l").await.unwrap();
        assert_eq!(volume.read_link("/a/l").await.unwrap(), Path::new("/etc"));
        // A link on the way is never followed out of the volume.
        assert!(volume.lstat("/a/l/passwd").await.unwrap().is_none());
        volume.rename("/a/b/f", "/a/g").await.unwrap();
        let names: Vec<String> = volume
            .read_dir("/a")
            .await
            .unwrap()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names.len(), 3);
        volume
            .set_attr(
                "/a/g",
                Some(0o600),
                Some(UNIX_EPOCH + Duration::from_secs(7)),
            )
            .await
            .unwrap();
        let meta = volume.lstat("/a/g").await.unwrap().unwrap();
        assert_eq!(
            (meta.mode, meta.modified),
            (0o600, UNIX_EPOCH + Duration::from_secs(7))
        );
        volume.remove_all("/a").await.unwrap();
        assert!(volume.lstat("/a").await.unwrap().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    /// The trait's own depth-first removal, which tenantd volumes use.
    #[tokio::test]
    async fn remove_all_from_the_calls_alone() {
        let dir = temp("remove-all");
        let volume = LocalVolume::new(&dir);
        volume.mkdir("/t/x/y", None, true).await.unwrap();
        volume.write("/t/x/y/f", b"1", None).await.unwrap();
        volume.write("/t/g", b"2", None).await.unwrap();
        volume.symlink("/nowhere", "/t/l").await.unwrap();
        remove_all(&volume, "/t").await.unwrap();
        assert!(!dir.join("t").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
