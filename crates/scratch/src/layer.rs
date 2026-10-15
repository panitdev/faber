//! Scratch's writable layer: everything the session changed on top of the base
//! image, kept on disk.
//!
//! On disk the layer is a directory tree (`upper/`) holding the files the
//! session wrote, plus `meta.json` holding what a plain tree cannot: which
//! base-image paths were deleted (whiteouts) and which directories hide the
//! base directory under them entirely (opaque directories). Unprivileged
//! processes cannot create the character devices overlayfs uses for
//! whiteouts, which is why they live in a sidecar instead of in the tree.
//!
//! Between modes the layer travels as a tar in **gVisor's own format**, the
//! one `runsc tar rootfs-upper` writes and the `dev.gvisor.tar.rootfs.upper`
//! annotation reads (verified against gVisor's `tmpfs/tar.go` and by running
//! both against each other):
//!
//! - names are `./`-prefixed, directories end in `/`, the root is `./`, and
//!   every directory appears before anything inside it;
//! - a deleted path is a character device with major and minor 0, and
//!   gVisor writes every deleted path after the first as a hard link to the
//!   first (they are one device to it);
//! - an opaque directory carries the PAX record
//!   `SCHILY.xattr.trusted.overlay.opaque=y`.
//!
//! [`Layer::export_tar`] writes exactly that and [`Layer::import_tar`] reads it
//! back, so a file deleted from the base in one mode stays deleted in the
//! other.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::beneath::{self, Beneath};

/// The PAX record gVisor's overlay reads and writes for an opaque directory.
pub const OPAQUE_PAX: &str = "SCHILY.xattr.trusted.overlay.opaque";

/// What the tree alone cannot say. Paths are absolute scratch paths.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Meta {
    /// Base-image paths deleted in this layer.
    whiteouts: BTreeSet<String>,
    /// Layer directories that hide the base directory at the same path.
    opaque: BTreeSet<String>,
}

/// A session's layer on disk.
pub struct Layer {
    dir: PathBuf,
    upper: PathBuf,
    meta: Mutex<Meta>,
}

impl Layer {
    /// Opens the layer under `dir`, creating an empty one if there is none.
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Layer> {
        let dir = dir.into();
        let upper = dir.join("upper");
        fs::create_dir_all(&upper)?;
        let meta = match fs::read(dir.join("meta.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Meta::default(),
            Err(error) => return Err(error),
        };
        Ok(Layer {
            dir,
            upper,
            meta: Mutex::new(meta),
        })
    }

    /// The directory the layer's tree is in.
    pub fn upper(&self) -> &Path {
        &self.upper
    }

    /// Where a scratch path lives in the tree. `path` is normalized and
    /// absolute; the result is never checked for existence.
    pub fn real(&self, path: &str) -> PathBuf {
        let relative = path.trim_start_matches('/');
        if relative.is_empty() {
            self.upper.clone()
        } else {
            self.upper.join(relative)
        }
    }

    pub fn is_whiteout(&self, path: &str) -> bool {
        self.meta().whiteouts.contains(path)
    }

    pub fn is_opaque(&self, path: &str) -> bool {
        self.meta().opaque.contains(path)
    }

    /// True when a directory strictly above `path` is opaque, so nothing at
    /// `path` comes from the base.
    pub fn under_opaque(&self, path: &str) -> bool {
        let meta = self.meta();
        ancestors(path).any(|ancestor| meta.opaque.contains(ancestor))
    }

    pub fn add_whiteout(&self, path: &str) -> io::Result<()> {
        self.update(|meta| {
            meta.whiteouts.insert(path.to_owned());
        })
    }

    pub fn clear_whiteout(&self, path: &str) -> io::Result<()> {
        if !self.is_whiteout(path) {
            return Ok(());
        }
        self.update(|meta| {
            meta.whiteouts.remove(path);
        })
    }

    pub fn set_opaque(&self, path: &str) -> io::Result<()> {
        self.update(|meta| {
            meta.opaque.insert(path.to_owned());
        })
    }

    /// Drops every whiteout and opaque mark at or under `path` — what removing
    /// that subtree from the layer leaves meaningless.
    pub fn forget(&self, path: &str) -> io::Result<()> {
        self.update(|meta| {
            meta.whiteouts.retain(|p| !is_at_or_under(p, path));
            meta.opaque.retain(|p| !is_at_or_under(p, path));
        })
    }

    /// Moves the marks at or under `from` to the same place under `to`, for a
    /// rename that happened inside the tree.
    pub fn rename_marks(&self, from: &str, to: &str) -> io::Result<()> {
        let rebase = |set: &mut BTreeSet<String>| {
            let moved: Vec<String> = set
                .iter()
                .filter(|p| is_at_or_under(p, from))
                .cloned()
                .collect();
            for old in moved {
                set.remove(&old);
                set.insert(format!("{to}{}", &old[from.len()..]));
            }
        };
        self.update(|meta| {
            meta.whiteouts.retain(|p| !is_at_or_under(p, to));
            meta.opaque.retain(|p| !is_at_or_under(p, to));
            rebase(&mut meta.whiteouts);
            rebase(&mut meta.opaque);
        })
    }

    /// Bytes this layer holds: file contents and symlink targets. What a quota
    /// counts — directories and metadata are noise next to them.
    pub fn bytes(&self) -> u64 {
        fn walk(dir: &Path) -> u64 {
            let Ok(entries) = fs::read_dir(dir) else {
                return 0;
            };
            entries
                .flatten()
                .map(|entry| match entry.metadata() {
                    Ok(meta) if meta.is_dir() => walk(&entry.path()),
                    Ok(meta) => meta.len(),
                    Err(_) => 0,
                })
                .sum()
        }
        walk(&self.upper)
    }

    /// Empties the layer: tree and marks.
    pub fn clear(&self) -> io::Result<()> {
        if self.upper.exists() {
            fs::remove_dir_all(&self.upper)?;
        }
        fs::create_dir_all(&self.upper)?;
        self.update(|meta| *meta = Meta::default())
    }

    /// Writes the layer as a tar gVisor applies as the rootfs upper layer.
    pub fn export_tar<W: Write>(&self, out: W) -> io::Result<W> {
        let meta = self.meta().clone();
        let mut entries: BTreeMap<String, Node> = BTreeMap::new();
        collect(&self.upper, "", &mut entries)?;
        for path in &meta.whiteouts {
            // A whiteout replaces whatever the tree has at that path; the
            // operations that create one remove the tree's copy first, so this
            // only matters for a layer edited by hand.
            entries.insert(path.clone(), Node::Whiteout);
        }
        // gVisor resolves each entry's parent by name, so every ancestor must
        // be in the archive — including those of a whiteout whose directory
        // exists only in the base.
        let paths: Vec<String> = entries.keys().cloned().collect();
        for path in paths {
            for ancestor in ancestors(&path) {
                if ancestor != "/" {
                    entries.entry(ancestor.to_owned()).or_insert(Node::Dir {
                        mode: 0o755,
                        mtime: 0,
                    });
                }
            }
        }

        let mut builder = tar::Builder::new(out);
        append(
            &mut builder,
            "./",
            &Node::Dir {
                mode: 0o755,
                mtime: 0,
            },
            false,
        )?;
        for (path, node) in &entries {
            let opaque = meta.opaque.contains(path);
            let name = match node {
                Node::Dir { .. } => format!(".{path}/"),
                _ => format!(".{path}"),
            };
            append(&mut builder, &name, node, opaque)?;
        }
        builder.into_inner()
    }

    /// Replaces the layer with the contents of a tar in gVisor's format.
    ///
    /// Entries scratch cannot represent unprivileged — devices other than a
    /// whiteout, FIFOs — are skipped with a warning. A hard link becomes a copy
    /// of the file it names, made once everything else is in, so a link that
    /// comes before its target still finds it.
    ///
    /// Every entry is written through directories opened without following
    /// symlinks: an entry under a name an earlier entry made a symlink is
    /// skipped, never written where the symlink points.
    pub fn import_tar<R: Read>(&self, input: R) -> io::Result<()> {
        self.clear()?;
        let mut meta = Meta::default();
        let mut links: Vec<(String, String)> = Vec::new();
        let mut archive = tar::Archive::new(input);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let raw = entry.path_bytes().into_owned();
            let Some(path) = archive_path(&raw) else {
                tracing::warn!(name = %String::from_utf8_lossy(&raw), "skipping a layer entry with an unusable name");
                continue;
            };
            let header = entry.header().clone();
            let mode = header.mode().unwrap_or(0o644) & 0o7777;
            let opaque = entry
                .pax_extensions()?
                .map(|pax| {
                    pax.flatten()
                        .any(|ext| ext.key() == Ok(OPAQUE_PAX) && ext.value_bytes() == b"y")
                })
                .unwrap_or(false);
            if path == "/" {
                continue;
            }
            let kind = header.entry_type();
            let real = match kind {
                tar::EntryType::Directory
                | tar::EntryType::Regular
                | tar::EntryType::Continuous
                | tar::EntryType::Symlink => match Beneath::create(&self.upper, &path) {
                    Ok(real) => Some(real),
                    Err(error) if error.raw_os_error() == Some(libc::ENOTDIR) => {
                        tracing::warn!(%path, "skipping a layer entry under a symlink");
                        continue;
                    }
                    Err(error) => return Err(error),
                },
                _ => None,
            };
            match kind {
                tar::EntryType::Directory => {
                    let real = real.expect("made above");
                    match fs::create_dir(&real) {
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        other => other?,
                    }
                    beneath::set_mode(&real, mode | 0o700)?;
                    if opaque {
                        meta.opaque.insert(path);
                    }
                }
                tar::EntryType::Regular | tar::EntryType::Continuous => {
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(real.expect("made above"))?;
                    io::copy(&mut entry, &mut file)?;
                    file.set_permissions(fs::Permissions::from_mode(mode | 0o600))?;
                    if let Ok(mtime) = header.mtime() {
                        let _ =
                            file.set_modified(UNIX_EPOCH + std::time::Duration::from_secs(mtime));
                    }
                }
                tar::EntryType::Symlink => {
                    let target = entry
                        .link_name_bytes()
                        .map(|bytes| bytes.into_owned())
                        .unwrap_or_default();
                    let target = String::from_utf8(target)
                        .map_err(|_| io::Error::other("a symlink target is not utf-8"))?;
                    symlink(target, real.expect("made above"))?;
                }
                tar::EntryType::Link => {
                    let target = entry
                        .link_name_bytes()
                        .and_then(|bytes| archive_path(&bytes));
                    match target {
                        // gVisor writes every whiteout after the first as a
                        // hard link to it: they are all the same 0/0 device.
                        Some(target) if meta.whiteouts.contains(&target) => {
                            meta.whiteouts.insert(path);
                        }
                        Some(target) => links.push((path, target)),
                        None => {
                            tracing::warn!(%path, "skipping a hard link with an unusable target")
                        }
                    }
                }
                tar::EntryType::Char
                    if header.device_major()? == Some(0) && header.device_minor()? == Some(0) =>
                {
                    meta.whiteouts.insert(path);
                }
                other => {
                    tracing::warn!(%path, kind = ?other, "skipping a layer entry scratch cannot hold")
                }
            }
        }
        for (path, target) in links {
            let copied = Beneath::new(&self.upper, &target).and_then(|from| {
                let to = Beneath::create(&self.upper, &path)?;
                beneath::copy_file(&from, &to)
            });
            if let Err(error) = copied {
                tracing::warn!(%path, %target, %error, "skipping a hard link scratch could not copy");
            }
        }
        self.update(|current| *current = meta)
    }

    fn meta(&self) -> std::sync::MutexGuard<'_, Meta> {
        self.meta.lock().expect("layer meta poisoned")
    }

    /// Applies a change to the marks and persists them before returning, so a
    /// crash never leaves the tree and its marks disagreeing for longer than
    /// one operation.
    fn update(&self, change: impl FnOnce(&mut Meta)) -> io::Result<()> {
        let mut meta = self.meta();
        change(&mut meta);
        let bytes = serde_json::to_vec(&*meta).map_err(io::Error::other)?;
        let staged = self.dir.join("meta.json.new");
        fs::write(&staged, bytes)?;
        fs::rename(staged, self.dir.join("meta.json"))
    }
}

/// One archive entry before it is written.
enum Node {
    Dir {
        mode: u32,
        mtime: u64,
    },
    File {
        real: PathBuf,
        mode: u32,
        mtime: u64,
        size: u64,
    },
    Symlink {
        target: Vec<u8>,
        mtime: u64,
    },
    Whiteout,
}

fn collect(dir: &Path, prefix: &str, into: &mut BTreeMap<String, Node>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            tracing::warn!(name = ?entry.file_name(), "skipping a non-utf-8 name in the layer");
            continue;
        };
        let path = format!("{prefix}/{name}");
        let meta = fs::symlink_metadata(entry.path())?;
        let mtime = meta.mtime().max(0) as u64;
        if meta.file_type().is_symlink() {
            let target = fs::read_link(entry.path())?;
            into.insert(
                path,
                Node::Symlink {
                    target: target.into_os_string().into_encoded_bytes(),
                    mtime,
                },
            );
        } else if meta.is_dir() {
            into.insert(
                path.clone(),
                Node::Dir {
                    mode: meta.mode() & 0o7777,
                    mtime,
                },
            );
            collect(&entry.path(), &path, into)?;
        } else if meta.is_file() {
            into.insert(
                path,
                Node::File {
                    real: entry.path(),
                    mode: meta.mode() & 0o7777,
                    mtime,
                    size: meta.len(),
                },
            );
        }
    }
    Ok(())
}

/// Writes one entry. The exact name goes in a PAX `path` record: the `tar`
/// crate normalizes the `./` prefix out of header names, and gVisor matches
/// parents by the literal `./dir/` spelling.
fn append<W: Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    node: &Node,
    opaque: bool,
) -> io::Result<()> {
    let mut records: Vec<(&str, Vec<u8>)> = vec![("path", name.as_bytes().to_vec())];
    if opaque {
        records.push((OPAQUE_PAX, b"y".to_vec()));
    }
    if let Node::Symlink { target, .. } = node {
        records.push(("linkpath", target.clone()));
    }
    let pax = pax_body(&records);
    let mut pax_header = tar::Header::new_ustar();
    pax_header.set_path("PaxHeader")?;
    pax_header.set_entry_type(tar::EntryType::XHeader);
    pax_header.set_size(pax.len() as u64);
    pax_header.set_mode(0o644);
    pax_header.set_cksum();
    builder.append(&pax_header, pax.as_slice())?;

    let mut header = tar::Header::new_ustar();
    // A placeholder the PAX record overrides; short so it always fits.
    header.set_path("entry")?;
    header.set_uid(0);
    header.set_gid(0);
    match node {
        Node::Dir { mode, mtime } => {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(*mode);
            header.set_mtime(*mtime);
            header.set_size(0);
            header.set_cksum();
            builder.append(&header, io::empty())
        }
        Node::File {
            real,
            mode,
            mtime,
            size,
        } => {
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(*mode);
            header.set_mtime(*mtime);
            header.set_size(*size);
            header.set_cksum();
            let file = fs::File::open(real)?;
            // Bound the read to the size in the header: a file that grew since
            // it was stat'ed must not corrupt the archive.
            builder.append(&header, file.take(*size))
        }
        Node::Symlink { mtime, .. } => {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_mode(0o777);
            header.set_mtime(*mtime);
            header.set_size(0);
            header.set_cksum();
            builder.append(&header, io::empty())
        }
        Node::Whiteout => {
            header.set_entry_type(tar::EntryType::Char);
            header.set_mode(0);
            header.set_device_major(0)?;
            header.set_device_minor(0)?;
            header.set_size(0);
            header.set_cksum();
            builder.append(&header, io::empty())
        }
    }
}

/// PAX records: `"<len> <key>=<value>\n"`, where `len` counts itself.
fn pax_body(records: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (key, value) in records {
        let rest = key.len() + value.len() + 3; // ' ', '=', '\n'
        let mut len = rest + 1;
        while len != rest + len.to_string().len() {
            len = rest + len.to_string().len();
        }
        body.extend_from_slice(format!("{len} {key}=").as_bytes());
        body.extend_from_slice(value);
        body.push(b'\n');
    }
    body
}

/// An archive name as a normalized absolute scratch path, or `None` for a
/// name that is absolute, walks upward, or is not utf-8.
fn archive_path(raw: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(raw).ok()?;
    if name.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            part => parts.push(part),
        }
    }
    Some(format!("/{}", parts.join("/")))
}

/// `path`'s ancestors, nearest first, ending with `/`.
pub(crate) fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let mut current = path;
    std::iter::from_fn(move || {
        if current == "/" || current.is_empty() {
            return None;
        }
        current = match current.rfind('/') {
            Some(0) => "/",
            Some(at) => &current[..at],
            None => return None,
        };
        Some(current)
    })
}

pub(crate) fn is_at_or_under(path: &str, root: &str) -> bool {
    root == "/"
        || path == root
        || (path.len() > root.len()
            && path.starts_with(root)
            && path.as_bytes()[root.len()] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("faber-layer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ancestors_walk_up_to_the_root() {
        assert_eq!(ancestors("/a/b/c").collect::<Vec<_>>(), ["/a/b", "/a", "/"]);
        assert_eq!(ancestors("/a").collect::<Vec<_>>(), ["/"]);
        assert_eq!(ancestors("/").count(), 0);
    }

    #[test]
    fn pax_lengths_count_their_own_digits() {
        let body = pax_body(&[("path", b"./a".to_vec())]);
        assert_eq!(body, b"12 path=./a\n");
        let long = "x".repeat(95);
        let body = pax_body(&[("path", long.as_bytes().to_vec())]);
        let text = String::from_utf8(body.clone()).unwrap();
        let declared: usize = text.split(' ').next().unwrap().parse().unwrap();
        assert_eq!(declared, body.len());
    }

    #[test]
    fn a_layer_round_trips_through_gvisors_tar_format() {
        let dir = scratch_dir("roundtrip");
        let layer = Layer::open(dir.join("a")).unwrap();
        fs::create_dir_all(layer.real("/etc")).unwrap();
        fs::write(layer.real("/etc/added"), b"added\n").unwrap();
        fs::set_permissions(layer.real("/etc/added"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(layer.real("/data/sub")).unwrap();
        symlink("/etc/added", layer.real("/data/link")).unwrap();
        layer.add_whiteout("/etc/motd").unwrap();
        layer.add_whiteout("/only/in/base").unwrap();
        layer.set_opaque("/data/sub").unwrap();

        let bytes = layer.export_tar(Vec::new()).unwrap();

        // The spelling gVisor needs, read back by an independent reader.
        let mut names = Vec::new();
        let mut archive = tar::Archive::new(bytes.as_slice());
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            names.push((
                String::from_utf8(entry.path_bytes().into_owned()).unwrap(),
                entry.header().entry_type(),
            ));
        }
        assert_eq!(names[0], ("./".to_owned(), tar::EntryType::Directory));
        assert!(names.contains(&("./etc/".to_owned(), tar::EntryType::Directory)));
        assert!(names.contains(&("./etc/motd".to_owned(), tar::EntryType::Char)));
        assert!(names.contains(&("./only/in/".to_owned(), tar::EntryType::Directory)));
        let position = |name: &str| names.iter().position(|(n, _)| n == name).unwrap();
        assert!(position("./only/") < position("./only/in/"));
        assert!(position("./only/in/") < position("./only/in/base"));

        let copy = Layer::open(dir.join("b")).unwrap();
        copy.import_tar(bytes.as_slice()).unwrap();
        assert_eq!(fs::read(copy.real("/etc/added")).unwrap(), b"added\n");
        assert_eq!(
            fs::metadata(copy.real("/etc/added")).unwrap().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_link(copy.real("/data/link")).unwrap(),
            PathBuf::from("/etc/added")
        );
        assert!(copy.is_whiteout("/etc/motd"));
        assert!(copy.is_whiteout("/only/in/base"));
        assert!(copy.is_opaque("/data/sub"));
        assert!(!copy.is_opaque("/data"));

        // Marks survive reopening.
        drop(copy);
        let reopened = Layer::open(dir.join("b")).unwrap();
        assert!(reopened.is_whiteout("/etc/motd"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn whiteouts_written_as_hard_links_are_whiteouts() {
        // What `runsc tar rootfs-upper` writes for two deletions.
        let mut builder = tar::Builder::new(Vec::new());
        let mut entry = |name: &str, kind: tar::EntryType, link: Option<&str>| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(0);
            header.set_mode(if kind == tar::EntryType::Directory {
                0o755
            } else {
                0
            });
            if kind == tar::EntryType::Char {
                header.set_device_major(0).unwrap();
                header.set_device_minor(0).unwrap();
            }
            if let Some(link) = link {
                header.set_link_name(link).unwrap();
            }
            header.set_path(name).unwrap();
            header.set_cksum();
            builder.append(&header, io::empty()).unwrap();
        };
        entry("./", tar::EntryType::Directory, None);
        entry("./etc/", tar::EntryType::Directory, None);
        entry("./etc/motd", tar::EntryType::Char, None);
        entry("./work/", tar::EntryType::Directory, None);
        entry("./work/a.txt", tar::EntryType::Link, Some("./etc/motd"));
        let bytes = builder.into_inner().unwrap();

        let dir = scratch_dir("hardlinks");
        let layer = Layer::open(&dir).unwrap();
        layer.import_tar(bytes.as_slice()).unwrap();
        assert!(layer.is_whiteout("/etc/motd"));
        assert!(layer.is_whiteout("/work/a.txt"));
        assert!(!layer.real("/work/a.txt").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_archive_never_writes_through_a_symlink_and_links_find_later_targets() {
        let dir = scratch_dir("through-links");
        let outside = dir.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut entry = |name: &str, kind: tar::EntryType, link: Option<&str>, body: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            if let Some(link) = link {
                header.set_link_name(link).unwrap();
            }
            header.set_path(name).unwrap();
            header.set_cksum();
            builder.append(&header, body).unwrap();
        };
        entry("./", tar::EntryType::Directory, None, b"");
        let target = outside.to_str().unwrap().to_owned();
        entry("./a", tar::EntryType::Symlink, Some(&target), b"");
        entry("./a/planted", tar::EntryType::Regular, None, b"host");
        entry("./a/sub/", tar::EntryType::Directory, None, b"");
        // A hard link ahead of the file it names.
        entry("./early", tar::EntryType::Link, Some("./late"), b"");
        entry("./late", tar::EntryType::Regular, None, b"body");
        let bytes = builder.into_inner().unwrap();

        let layer = Layer::open(dir.join("layer")).unwrap();
        layer.import_tar(bytes.as_slice()).unwrap();
        assert!(!outside.join("planted").exists());
        assert!(!outside.join("sub").exists());
        assert!(
            fs::symlink_metadata(layer.real("/a"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(layer.real("/early")).unwrap(), b"body");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_that_escape_are_not_imported() {
        assert_eq!(archive_path(b"./a/b/"), Some("/a/b".into()));
        assert_eq!(archive_path(b"./"), Some("/".into()));
        assert_eq!(archive_path(b"a/../../etc"), None);
        assert_eq!(archive_path(b"/etc/passwd"), None);
    }

    #[test]
    fn renaming_moves_marks_under_the_new_name() {
        let dir = scratch_dir("marks");
        let layer = Layer::open(&dir).unwrap();
        layer.add_whiteout("/a/x").unwrap();
        layer.set_opaque("/a").unwrap();
        layer.add_whiteout("/ab").unwrap();
        layer.rename_marks("/a", "/b").unwrap();
        assert!(layer.is_whiteout("/b/x"));
        assert!(layer.is_opaque("/b"));
        assert!(!layer.is_whiteout("/a/x"));
        assert!(
            layer.is_whiteout("/ab"),
            "a sibling sharing a prefix is not under /a"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
