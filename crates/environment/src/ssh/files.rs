//! Files on the far side, over the SFTP subsystem.
//!
//! SFTP is a subsystem of the SSH server, not a program in the user's `PATH`,
//! so `read`, `write`, and `list` behave the same whatever the machine has
//! installed — the same reason the container daemon's archive endpoints are
//! admissible where `docker exec cat` is not.
//!
//! It also supplies the one thing this mode needs and a lexical check cannot
//! give: `REALPATH` resolves symlinks on the far side, which is what catches a
//! link inside the root pointing out of it. That check is the whole difference
//! between a root that is enforced and one that is merely declared, and on a
//! plain host nothing underneath enforces it.

use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::FileType;
use tokio::io::AsyncWriteExt;

use crate::fault::{Denial, Fault};
use crate::file::{EntryKind, Stat};
use crate::files::{Confined, DirEntry, Files};
use crate::path::{Root, RootedPath};
use crate::ssh::SshSession;

pub struct SftpFiles {
    sftp: SftpSession,
    /// The root as the far side resolves it, for the symlink re-check.
    real_root: String,
}

impl SftpFiles {
    /// Opens the SFTP subsystem on its own channel over an existing session.
    pub async fn open(session: Arc<SshSession>, root: Root) -> Result<Self, Fault> {
        let channel = session
            .handle()
            .channel_open_session()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|error| {
                Fault::Unreachable(format!("the far side refused an sftp subsystem: {error}"))
            })?;

        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|error| Fault::Unreachable(format!("could not start sftp: {error}")))?;

        let real_root = sftp.canonicalize(root.as_str()).await.map_err(|error| {
            Fault::Denied(Denial::NotFound {
                path: format!("{root}: {error}"),
            })
        })?;

        Ok(SftpFiles { sftp, real_root })
    }

    /// Resolves a path on the far side and refuses anything that leaves the
    /// root once symlinks are followed.
    ///
    /// A path that does not exist yet cannot be canonicalized, so its deepest
    /// existing ancestor is resolved and the rest appended — the same shape
    /// the local mode uses, and for the same reason: a write to a new file has
    /// to be confined by the same check as a read of an existing one.
    async fn real(&self, path: &RootedPath) -> Result<(String, bool), Fault> {
        let mut existing = path.as_str().to_owned();
        let mut trailing: Vec<String> = Vec::new();

        let resolved = loop {
            match self.sftp.canonicalize(existing.clone()).await {
                Ok(real) => break real,
                Err(_) => {
                    let Some(at) = existing.rfind('/') else {
                        return Err(escape(path));
                    };
                    trailing.push(existing[at + 1..].to_owned());
                    existing.truncate(at.max(1));
                }
            }
        };

        let whole = trailing.is_empty();
        let mut full = resolved;
        for name in trailing.iter().rev() {
            if !full.ends_with('/') {
                full.push('/');
            }
            full.push_str(name);
        }

        if !full.starts_with(&self.real_root) {
            return Err(escape(path));
        }
        Ok((full, whole))
    }
}

#[async_trait]
impl Files for SftpFiles {
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault> {
        let (real, exists) = self.real(path).await?;
        let kind = if exists {
            self.sftp
                .metadata(real.clone())
                .await
                .ok()
                .map(|meta| kind_of(meta.file_type()))
        } else {
            None
        };
        Ok(Confined { path: real, kind })
    }

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        let (real, _) = self.real(path).await?;
        self.sftp
            .read(real)
            .await
            .map_err(|error| missing(path, &error))
    }

    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        let (real, _) = self.real(path).await?;

        // SFTP has no mkdir -p, so the ancestors are created one at a time and
        // an already-existing one is not an error.
        if let Some(at) = real.rfind('/') {
            let mut so_far = String::new();
            for part in real[..at].split('/').filter(|part| !part.is_empty()) {
                so_far.push('/');
                so_far.push_str(part);
                let _ = self.sftp.create_dir(so_far.clone()).await;
            }
        }

        // `SftpSession::write` opens with WRITE alone, which fails outright on
        // a file that does not exist yet and leaves a tail behind when the new
        // contents are shorter than the old. `create` is CREATE|TRUNCATE|WRITE,
        // which is what writing a file whole means everywhere else in the crate.
        let mut file = self
            .sftp
            .create(real.clone())
            .await
            .map_err(|error| missing(path, &error))?;
        file.write_all(body)
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        file.flush()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;

        Ok(Stat {
            path: path.clone(),
            size: body.len() as u64,
            modified: self
                .sftp
                .metadata(real)
                .await
                .ok()
                .and_then(|meta| meta.modified().ok())
                .or_else(|| Some(SystemTime::now())),
        })
    }

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault> {
        let (real, _) = self.real(path).await?;
        self.sftp
            .remove_file(real)
            .await
            .map_err(|error| missing(path, &error))
    }

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault> {
        let (source, _) = self.real(from).await?;
        let (destination, _) = self.real(to).await?;

        if let Some(at) = destination.rfind('/') {
            let mut so_far = String::new();
            for part in destination[..at].split('/').filter(|part| !part.is_empty()) {
                so_far.push('/');
                so_far.push_str(part);
                let _ = self.sftp.create_dir(so_far.clone()).await;
            }
        }

        self.sftp
            .rename(source, destination.clone())
            .await
            .map_err(|error| missing(from, &error))?;

        let meta = self.sftp.metadata(destination).await.ok();
        Ok(Stat {
            path: to.clone(),
            size: meta.as_ref().and_then(|meta| meta.size).unwrap_or(0),
            modified: Some(SystemTime::now()),
        })
    }

    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault> {
        let (real, _) = self.real(dir).await?;
        let listing = self
            .sftp
            .read_dir(real)
            .await
            .map_err(|error| missing(dir, &error))?;

        Ok(listing
            .map(|entry| DirEntry {
                name: entry.file_name(),
                kind: kind_of(entry.file_type()),
                size: entry.metadata().size,
            })
            // `.` and `..` are the protocol's, not the directory's.
            .filter(|entry| entry.name != "." && entry.name != "..")
            .collect())
    }
}

fn kind_of(kind: FileType) -> EntryKind {
    match kind {
        FileType::Dir => EntryKind::Dir,
        FileType::File => EntryKind::File,
        FileType::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

fn escape(path: &RootedPath) -> Fault {
    Fault::Denied(Denial::PathEscape {
        path: path.to_string(),
        reason: "resolves outside the target's root via a symlink".into(),
    })
}

/// A missing path is a denial naming the path; anything else is the
/// transport's problem rather than the request's.
fn missing(path: &RootedPath, error: &russh_sftp::client::error::Error) -> Fault {
    let text = error.to_string();
    if text.contains("No such file") || text.contains("NoSuchFile") {
        Fault::Denied(Denial::NotFound {
            path: path.to_string(),
        })
    } else {
        Fault::Unreachable(text)
    }
}
