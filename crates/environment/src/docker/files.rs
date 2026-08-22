//! Files inside a container, through the daemon rather than through the image.
//!
//! The archive endpoints are the reason this is admissible at all: they move a
//! tar in and out of a container without running anything inside it, so
//! `read`, `write`, and `list` behave the same whether the image is Debian or
//! `scratch`. That is the property shelling out to `cat` would give up.
//!
//! **Two operations have no endpoint, and they are done with a shell.** The
//! Engine API can put a file into a container and take one out; it cannot
//! delete or rename one. So `remove` and `rename` run `rm -f` and `mv`. This
//! is a deliberate and bounded exception: the objection to using the image's
//! binaries is that their *output* varies — `ls` formatting, `find`
//! predicates, `sed -i` semantics — and nothing here reads output. Both
//! commands are POSIX, both are answered by an exit code, and both mean the
//! same thing on busybox and GNU. An image carrying neither loses two verbs
//! and keeps the rest, which is better than the alternative of losing them for
//! everyone.

use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use serde_json::Value;

use crate::docker::daemon::Daemon;
use crate::docker::engine;
use crate::fault::{Denial, Fault};
use crate::file::{EntryKind, Stat};
use crate::files::{Confined, DirEntry, Files};
use crate::path::RootedPath;

/// Go's `os.ModeDir`, as it appears in a container path stat.
const MODE_DIR: u64 = 1 << 31;
/// Go's `os.ModeSymlink`.
const MODE_SYMLINK: u64 = 1 << 27;

pub struct DockerFiles {
    daemon: Arc<dyn Daemon>,
    container: String,
}

impl DockerFiles {
    pub fn new(daemon: Arc<dyn Daemon>, container: impl Into<String>) -> Self {
        DockerFiles {
            daemon,
            container: container.into(),
        }
    }

    /// Runs a command for its exit code alone.
    ///
    /// Nothing reads its output — see this module's header for why that
    /// distinction is the whole justification.
    async fn run(&self, script: String, about: &RootedPath) -> Result<(), Fault> {
        let cmd = vec!["/bin/sh".to_owned(), "-c".to_owned(), script];
        let exec =
            engine::exec_create(&self.daemon, &self.container, &cmd, &[], "/", false).await?;
        let stream = engine::exec_start(&self.daemon, &exec, false).await?;
        drop(stream);

        loop {
            let state = engine::exec_inspect(&self.daemon, &exec).await?;
            if !state.running {
                return match state.exit_code {
                    Some(0) => Ok(()),
                    _ => Err(Fault::Denied(Denial::NotFound {
                        path: about.to_string(),
                    })),
                };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[async_trait]
impl Files for DockerFiles {
    /// Lexical refusal is the whole check here.
    ///
    /// The container is the boundary: a symlink pointing out of the root
    /// points at something else inside the container, and the archive endpoint
    /// will not follow one out of the container's filesystem. There is nothing
    /// left for this layer to enforce, which is exactly what a target
    /// publishing an enforced posture is claiming.
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault> {
        let stat = engine::archive_stat(&self.daemon, &self.container, path.as_str()).await?;
        Ok(Confined {
            path: path.as_str().to_owned(),
            kind: stat.as_ref().map(kind_of),
        })
    }

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        let tar = engine::archive_get(&self.daemon, &self.container, path.as_str()).await?;
        single_file(&tar).map_err(|_| {
            Fault::Denied(Denial::Malformed {
                what: "read".into(),
                reason: format!("`{path}` is not a regular file"),
            })
        })
    }

    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        // The tar carries the path relative to `/`, and the daemon unpacks it
        // there, so a write to a nested path needs no directory to exist
        // beforehand — the archive names the directories it needs.
        let relative = path.as_str().trim_start_matches('/');
        let tar = one_file_tar(relative, body).map_err(|error| {
            Fault::Unreachable(format!("could not build an archive for `{path}`: {error}"))
        })?;
        engine::archive_put(&self.daemon, &self.container, "/", &tar).await?;

        Ok(Stat {
            path: path.clone(),
            size: body.len() as u64,
            modified: Some(SystemTime::now()),
        })
    }

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault> {
        self.run(format!("rm -f -- '{}'", quote(path.as_str())), path)
            .await
    }

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault> {
        let parent = parent_of(to.as_str());
        self.run(
            format!(
                "mkdir -p -- '{}' && mv -- '{}' '{}'",
                quote(&parent),
                quote(from.as_str()),
                quote(to.as_str())
            ),
            from,
        )
        .await?;

        let stat = engine::archive_stat(&self.daemon, &self.container, to.as_str()).await?;
        Ok(Stat {
            path: to.clone(),
            size: stat
                .as_ref()
                .and_then(|value| value.get("size")?.as_u64())
                .unwrap_or(0),
            modified: Some(SystemTime::now()),
        })
    }

    /// One directory's entries, out of the tar of its subtree.
    ///
    /// The endpoint has no depth control, so this pulls more than it needs and
    /// keeps the top level. Correct and image-independent, but linear in the
    /// size of the subtree rather than in the number of entries — the honest
    /// trade for not parsing `ls` output. See the transports document's open
    /// questions.
    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault> {
        let tar = engine::archive_get(&self.daemon, &self.container, dir.as_str()).await?;
        let mut archive = tar::Archive::new(std::io::Cursor::new(tar));
        let entries = archive
            .entries()
            .map_err(|error| Fault::Unreachable(error.to_string()))?;

        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| Fault::Unreachable(error.to_string()))?;
            let path = entry
                .path()
                .map_err(|error| Fault::Unreachable(error.to_string()))?
                .to_string_lossy()
                .into_owned();

            // The daemon prefixes every entry with the requested directory's
            // own name, so a direct child has exactly two components.
            let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
            if parts.len() != 2 {
                continue;
            }

            found.push(DirEntry {
                name: parts[1].to_owned(),
                kind: match entry.header().entry_type() {
                    tar::EntryType::Directory => EntryKind::Dir,
                    tar::EntryType::Regular => EntryKind::File,
                    tar::EntryType::Symlink | tar::EntryType::Link => EntryKind::Symlink,
                    _ => EntryKind::Other,
                },
                size: entry.header().size().ok(),
            });
        }
        Ok(found)
    }
}

/// The bytes of the single regular file in a one-entry archive.
pub(crate) fn single_file(tar: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut archive = tar::Archive::new(std::io::Cursor::new(tar));
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type() == tar::EntryType::Regular {
            let mut body = Vec::new();
            entry.read_to_end(&mut body)?;
            return Ok(body);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "the archive holds no regular file",
    ))
}

/// An archive holding one file, plus the directories it needs on the way.
fn one_file_tar(relative: &str, body: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let mut builder = tar::Builder::new(Vec::new());

    // Naming each ancestor explicitly rather than relying on the unpacker to
    // invent them: what a tar extractor does with a missing parent varies, and
    // this is one of the places that variation would be invisible until a
    // nested write failed.
    let parts: Vec<&str> = relative.split('/').collect();
    let mut so_far = String::new();
    for part in &parts[..parts.len().saturating_sub(1)] {
        so_far.push_str(part);
        so_far.push('/');
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_mtime(0);
        builder.append_data(&mut header, &so_far, std::io::empty())?;
    }

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    builder.append_data(&mut header, relative, body)?;

    builder.into_inner()
}

fn kind_of(stat: &Value) -> EntryKind {
    let mode = stat.get("mode").and_then(Value::as_u64).unwrap_or(0);
    if mode & MODE_DIR != 0 {
        EntryKind::Dir
    } else if mode & MODE_SYMLINK != 0 {
        EntryKind::Symlink
    } else {
        EntryKind::File
    }
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(at) => path[..at].to_owned(),
    }
}

/// Makes a path safe inside single quotes.
///
/// Paths reach the shell only in `remove` and `rename`, and both are refused
/// well before here if they left the root — but a filename containing a quote
/// is ordinary, not an attack, and it should move rather than break.
fn quote(path: &str) -> String {
    path.replace('\'', r"'\''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_path_survives_a_single_quote_in_a_filename() {
        assert_eq!(quote("/work/it's.txt"), r"/work/it'\''s.txt");
    }

    #[test]
    fn an_archive_round_trips_one_file() {
        let tar = one_file_tar("work/sub/file.txt", b"hello").unwrap();
        assert_eq!(single_file(&tar).unwrap(), b"hello");
    }

    #[test]
    fn an_archive_names_every_directory_on_the_way() {
        let tar = one_file_tar("a/b/c.txt", b"x").unwrap();
        let mut archive = tar::Archive::new(std::io::Cursor::new(tar));
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["a/", "a/b/", "a/b/c.txt"]);
    }

    #[test]
    fn a_directory_stat_reads_as_a_directory() {
        assert_eq!(
            kind_of(&serde_json::json!({ "mode": MODE_DIR | 0o755 })),
            EntryKind::Dir
        );
        assert_eq!(
            kind_of(&serde_json::json!({ "mode": 0o644 })),
            EntryKind::File
        );
    }
}
