//! Scratch's writable layer in virtual mode: everything a session changed on
//! top of the base image outside its project's volumes.
//!
//! On disk the layer is a directory tree (`upper/`) holding the files the
//! session wrote, plus `meta.json` holding what a plain tree cannot: which
//! base-image paths were deleted (whiteouts) and which directories hide the
//! base directory under them entirely (opaque directories). Unprivileged
//! processes cannot create the character devices overlayfs uses for
//! whiteouts, which is why they live in a sidecar instead of in the tree.
//!
//! The layer is ephemeral. It never crosses into compute, whose container
//! has a root of its own, and it is emptied when the session promotes and
//! when it goes idle: what a session keeps, it keeps in the workspace or in
//! `/tmp`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

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
