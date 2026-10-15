//! `read` and `patch` while scratch is virtual: the environment crate's
//! [`Files`] over [`ScratchFs`], so they see exactly what the virtual shell
//! sees — layer over base, the toolstore, and the mounts under `/mnt`.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bashkit::{FileSystem, FileType};
use environment::files::{Confined, DirEntry, Files};
use environment::{Denial, EntryKind, Fault, RootedPath, Stat};

use crate::vfs::{ScratchFs, parent_of};

pub struct VirtualFiles {
    pub fs: Arc<ScratchFs>,
}

/// A missing path is a denial naming it; anything else is the filesystem's
/// own failure, worded as the sandbox would word it.
fn fault(path: &RootedPath, error: bashkit::Error) -> Fault {
    match error {
        bashkit::Error::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            Fault::Denied(Denial::NotFound {
                path: path.to_string(),
            })
        }
        bashkit::Error::Io(io) => Fault::Unreachable(io.to_string()),
        other => Fault::Unreachable(other.to_string()),
    }
}

fn kind(file_type: FileType) -> EntryKind {
    match file_type {
        FileType::File => EntryKind::File,
        FileType::Directory => EntryKind::Dir,
        FileType::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

impl VirtualFiles {
    async fn stat(&self, path: &RootedPath) -> Result<Stat, Fault> {
        let meta = self
            .fs
            .stat(Path::new(path.as_str()))
            .await
            .map_err(|error| fault(path, error))?;
        Ok(Stat {
            path: path.clone(),
            size: meta.size,
            modified: Some(meta.modified),
        })
    }

    async fn parents(&self, path: &RootedPath) -> Result<(), Fault> {
        let parent = parent_of(path.as_str());
        self.fs
            .mkdir(Path::new(parent), true)
            .await
            .map_err(|error| fault(path, error))
    }
}

#[async_trait]
impl Files for VirtualFiles {
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault> {
        // Scratch is its own namespace; resolution can land anywhere in it
        // and nowhere outside, so all this does is follow links and look.
        let resolved = self
            .fs
            .resolve_now(path.as_str(), true, true)
            .unwrap_or_else(|_| path.as_str().to_owned());
        let kind = match self.fs.stat(Path::new(&resolved)).await {
            Ok(meta) => Some(kind(meta.file_type)),
            Err(_) => None,
        };
        Ok(Confined {
            path: resolved,
            kind,
        })
    }

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        self.fs
            .read_file(Path::new(path.as_str()))
            .await
            .map_err(|error| fault(path, error))
    }

    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        self.parents(path).await?;
        self.fs
            .write_file(Path::new(path.as_str()), body)
            .await
            .map_err(|error| fault(path, error))?;
        self.stat(path).await
    }

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault> {
        // A file, as everywhere else: removing a directory is a command's job.
        let meta = self
            .fs
            .stat(Path::new(path.as_str()))
            .await
            .map_err(|error| fault(path, error))?;
        if meta.file_type.is_dir() {
            return Err(Fault::Unreachable(format!(
                "`{path}` is a directory: Is a directory"
            )));
        }
        self.fs
            .remove(Path::new(path.as_str()), false)
            .await
            .map_err(|error| fault(path, error))
    }

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault> {
        self.parents(to).await?;
        self.fs
            .rename(Path::new(from.as_str()), Path::new(to.as_str()))
            .await
            .map_err(|error| fault(from, error))?;
        self.stat(to).await
    }

    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault> {
        let entries = self
            .fs
            .read_dir(Path::new(dir.as_str()))
            .await
            .map_err(|error| fault(dir, error))?;
        Ok(entries
            .into_iter()
            .map(|entry| DirEntry {
                name: entry.name,
                kind: kind(entry.metadata.file_type),
                size: Some(entry.metadata.size),
            })
            .collect())
    }
}
