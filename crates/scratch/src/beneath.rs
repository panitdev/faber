//! Host paths under a root that no symlink on the way can lead out of.
//!
//! A session's layer is an ordinary directory tree on the host, and the agent
//! can put a symlink anywhere in it, including one whose target is an
//! absolute host path. Scratch resolves symlinks itself, inside its own
//! namespace, so a path it hands to the host should never cross one. But the
//! kernel follows a symlink in every component of a path except, with
//! `O_NOFOLLOW`, the last; a directory that turned into a symlink after the
//! path was checked would take the operation out of the tree.
//!
//! [`Beneath`] closes that gap. It opens each directory on the way with
//! `O_NOFOLLOW`, keeps the last one open, and names the entry through that
//! open directory (`/proc/self/fd/<fd>/<name>`). A component that is a
//! symlink fails the walk with `ENOTDIR` instead of being followed, and
//! whatever happens to the path afterwards, the operation stays in the
//! directory that was opened. The last component keeps the meaning the
//! operation gives it, so operations that would follow a final symlink open
//! it with `O_NOFOLLOW` (see [`open_nofollow`], [`set_mode`], [`open_dir`]).

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// An entry under a root, named through its parent directory held open.
#[derive(Debug)]
pub struct Beneath {
    /// The directory `path` is named through. Never read; it only has to
    /// stay open as long as `path` is used.
    _dir: Option<OwnedFd>,
    path: PathBuf,
}

impl Beneath {
    /// `relative` under `root`, without creating anything. `relative` is a
    /// scratch-style path: components split on `/`, leading `/` ignored, the
    /// empty path being `root` itself. Fails with `ENOTDIR` when a directory
    /// on the way is a symlink or not a directory, and `ENOENT` when one is
    /// missing.
    pub fn new(root: &Path, relative: &str) -> io::Result<Beneath> {
        walk(root, relative, false)
    }

    /// Like [`new`](Beneath::new), creating missing directories on the way
    /// (mode 0755).
    pub fn create(root: &Path, relative: &str) -> io::Result<Beneath> {
        walk(root, relative, true)
    }
}

impl AsRef<Path> for Beneath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for Beneath {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

/// Whether `/proc/self/fd` can name a file through an open directory. Without
/// it (no procfs) the walk still checks every component, but the result is a
/// plain path again, which a concurrent change could redirect.
fn proc_fd() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let available = Path::new("/proc/self/fd").is_dir();
        if !available {
            tracing::warn!("/proc is not mounted; scratch's symlink containment is weaker");
        }
        available
    })
}

fn cstring(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn open_at(dir: Option<&OwnedFd>, name: &CString, flags: libc::c_int) -> io::Result<OwnedFd> {
    let at = dir.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    // SAFETY: `name` is a valid C string and `at` an open directory or
    // AT_FDCWD; the returned descriptor is owned by nothing else.
    let fd = unsafe { libc::openat(at, name.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and is not shared.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

const DIR_FLAGS: libc::c_int = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW;

fn walk(root: &Path, relative: &str, create: bool) -> io::Result<Beneath> {
    let parts: Vec<&str> = relative
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.contains(&"..") {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let Some((last, above)) = parts.split_last() else {
        return Ok(Beneath {
            _dir: None,
            path: root.to_path_buf(),
        });
    };
    // The root is the operator's, so it alone may be reached through a
    // symlink.
    let mut dir = open_at(
        None,
        &cstring(root.as_os_str().as_bytes())?,
        libc::O_PATH | libc::O_DIRECTORY,
    )?;
    for part in above {
        let name = cstring(part.as_bytes())?;
        let next = match open_at(Some(&dir), &name, DIR_FLAGS) {
            Err(error) if create && error.raw_os_error() == Some(libc::ENOENT) => {
                // SAFETY: `dir` is open and `name` a valid C string.
                let made = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) };
                if made < 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EEXIST) {
                        return Err(error);
                    }
                }
                open_at(Some(&dir), &name, DIR_FLAGS)
            }
            other => other,
        };
        dir = next.map_err(|error| match error.raw_os_error() {
            // A symlink where a directory should be reads as one that is not
            // a directory: there is nothing to follow.
            Some(libc::ELOOP) => io::Error::from_raw_os_error(libc::ENOTDIR),
            _ => error,
        })?;
    }
    if !proc_fd() {
        let mut path = root.to_path_buf();
        path.extend(&parts);
        return Ok(Beneath {
            _dir: Some(dir),
            path,
        });
    }
    let path = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(last);
    Ok(Beneath {
        _dir: Some(dir),
        path,
    })
}

/// Opens `path` for reading without following a final symlink.
pub fn open_nofollow(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// The directory at `path`, held open and named through its descriptor, so
/// listing it never follows a final symlink.
pub fn open_dir(path: &Path) -> io::Result<Beneath> {
    let dir = open_at(None, &cstring(path.as_os_str().as_bytes())?, DIR_FLAGS)?;
    let named = if proc_fd() {
        PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()))
    } else {
        path.to_path_buf()
    };
    Ok(Beneath {
        _dir: Some(dir),
        path: named,
    })
}

/// Sets the mode of the file or directory at `path`, refusing a final
/// symlink rather than changing what it points at.
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let handle = open_at(
        None,
        &cstring(path.as_os_str().as_bytes())?,
        libc::O_PATH | libc::O_NOFOLLOW,
    )?;
    let file = fs::File::from(handle);
    if file.metadata()?.file_type().is_symlink() {
        return Err(io::Error::from_raw_os_error(libc::ELOOP));
    }
    if !proc_fd() {
        return fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    // An `O_PATH` descriptor cannot be `fchmod`ed, but the file it holds can
    // be named through procfs, which is how glibc's own `fchmodat` with
    // `AT_SYMLINK_NOFOLLOW` does it.
    fs::set_permissions(
        format!("/proc/self/fd/{}", file.as_raw_fd()),
        fs::Permissions::from_mode(mode),
    )
}

/// Copies the regular file at `from` to a new or truncated file at `to`, with
/// `from`'s mode, following a final symlink at neither end.
pub fn copy_file(from: &Path, to: &Path) -> io::Result<u64> {
    let mut source = open_nofollow(from)?;
    let mode = source.metadata()?.permissions().mode() & 0o7777;
    let mut dest = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(to)?;
    let copied = io::copy(&mut source, &mut dest)?;
    dest.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("faber-beneath-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_symlink_on_the_way_is_never_followed() {
        let dir = temp("walk");
        let root = dir.join("root");
        let outside = dir.join("outside");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), b"host").unwrap();
        symlink(&outside, root.join("link")).unwrap();

        let error = Beneath::new(&root, "/link/secret").unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
        let error = Beneath::create(&root, "/link/new/file").unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTDIR));
        assert!(!outside.join("new").exists());

        // The last component is named as is: a final symlink is the link.
        let link = Beneath::new(&root, "/link").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(open_nofollow(&link).is_err());
        assert!(set_mode(&link, 0o700).is_err());
        assert!(open_dir(&link).is_err());

        let made = Beneath::create(&root, "/b/c/file").unwrap();
        fs::write(&made, b"x").unwrap();
        assert_eq!(fs::read(root.join("b/c/file")).unwrap(), b"x");
        assert!(Beneath::new(&root, "/../outside").is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_directory_swapped_for_a_symlink_after_the_walk_is_not_followed() {
        let dir = temp("swap");
        let root = dir.join("root");
        let outside = dir.join("outside");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let held = Beneath::new(&root, "/a/file").unwrap();
        // Another run replaces the directory with a link out of the tree.
        fs::rename(root.join("a"), root.join("moved")).unwrap();
        symlink(&outside, root.join("a")).unwrap();
        fs::write(&held, b"stays").unwrap();

        assert!(!outside.join("file").exists());
        assert_eq!(fs::read(root.join("moved/file")).unwrap(), b"stays");
        fs::remove_dir_all(dir).unwrap();
    }
}
