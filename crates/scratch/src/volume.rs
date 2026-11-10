//! A project's volume in tenantd, reached only through tenantd's `Files`
//! API, as the volume's owner. Nothing about it has to be on this host, so
//! tenantd may run anywhere the API can reach.

use std::fmt;
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tenantd_client::files::{self, Files};
use tenantd_client::pb;
use tonic::transport::Channel;

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
fn errno_error(code: i32) -> io::Error {
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

/// When a file tenantd describes was last modified.
pub(crate) fn modified(attr: &pb::FileAttr) -> SystemTime {
    if attr.mtime_ns >= 0 {
        UNIX_EPOCH + Duration::from_nanos(attr.mtime_ns as u64)
    } else {
        UNIX_EPOCH
    }
}

/// A volume in tenantd. Paths are absolute inside it: `/` is its top.
#[derive(Clone)]
pub struct TenantdVolume {
    files: Files,
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

    /// Whether anything is at `path`, without following a final symlink.
    pub async fn exists(&self, path: &str) -> io::Result<bool> {
        match self.files().stat(path, false).await {
            Ok(_) => Ok(true),
            Err(error) => {
                let error = tenantd_error(error);
                if gone(&error) { Ok(false) } else { Err(error) }
            }
        }
    }

    /// The names in a directory. A missing directory has none.
    pub async fn names(&self, path: &str) -> io::Result<Vec<String>> {
        match self.files().read_dir(path).await {
            Ok(entries) => Ok(entries.into_iter().map(|entry| entry.name).collect()),
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

    /// Replaces (or creates) the file with `data`; `mode` when given, else
    /// the replaced file's, or 0644.
    pub async fn write(&self, path: &str, data: &[u8], mode: Option<u32>) -> io::Result<()> {
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

    /// Makes a directory with `mode` (0755 when `None`). With `parents`,
    /// missing parents are made and an existing directory is no error.
    pub async fn mkdir(&self, path: &str, mode: Option<u32>, parents: bool) -> io::Result<()> {
        self.files()
            .mkdir(path, mode, parents)
            .await
            .map(|_| ())
            .map_err(tenantd_error)
    }
}
