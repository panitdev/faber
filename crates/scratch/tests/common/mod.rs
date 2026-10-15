//! Shared fixtures: a tiny base image, an in-memory mount backend, and a
//! manager over a fresh scratch root.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use environment::{Blobs, MemoryBlobs, Span};
use scratch::{Entry, FsError, Kind, MountBackend, ScratchConfig, ScratchManager};

/// A directory removed when dropped.
pub struct Temp(pub PathBuf);

impl Temp {
    pub fn new(name: &str) -> Temp {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "faber-scratch-{name}-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        // FABER_TEST_KEEP leaves the directory for a look after a failure.
        if std::env::var_os("FABER_TEST_KEEP").is_some() {
            eprintln!("kept {}", self.0.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A base image with what the tests probe: OS files, `/bin` as a symlink
/// into `/usr/bin` as on Debian, a work tree, and a non-empty `/tmp` (an
/// empty one gets a tmpfs under gVisor).
pub fn base_image(dir: &Path) {
    for sub in [
        "etc", "usr/bin", "work/sub", "mnt", "tmp", "root", "proc", "dev", "sys", "opt",
    ] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    symlink("usr/bin", dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("etc/os-release"),
        "PRETTY_NAME=\"Scratch Test\"\nID=test\n",
    )
    .unwrap();
    std::fs::write(dir.join("etc/motd"), "base motd\n").unwrap();
    std::fs::write(dir.join("tmp/.keep"), "").unwrap();
    std::fs::write(dir.join("work/a.txt"), "one\ntwo\nthree\ntwo\n").unwrap();
    std::fs::write(dir.join("work/sub/c.rs"), "fn main() {}\n").unwrap();
    let tool = dir.join("usr/bin/cargo");
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Adds a static busybox as `/usr/bin/*`, for an image compute can run.
pub fn add_busybox(dir: &Path, busybox: &Path) {
    let target = dir.join("usr/bin/busybox");
    std::fs::copy(busybox, &target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let list = std::process::Command::new(busybox)
        .arg("--list")
        .output()
        .unwrap();
    for name in String::from_utf8_lossy(&list.stdout).lines() {
        let link = dir.join("usr/bin").join(name);
        if !link.exists() && name != "busybox" {
            symlink("busybox", link).unwrap();
        }
    }
}

/// Logs to the test's output under `RUST_LOG`.
pub fn trace() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

pub fn blobs() -> Arc<dyn Blobs> {
    Arc::new(MemoryBlobs::new())
}

pub fn text(blobs: &Arc<dyn Blobs>, span: &Span) -> String {
    String::from_utf8(blobs.get(&span.blob).unwrap()).unwrap()
}

/// A scratch root with a base image, and a manager over it.
pub struct Fixture {
    pub dir: Temp,
    pub config: ScratchConfig,
}

impl Fixture {
    pub fn new(name: &str) -> Fixture {
        trace();
        let dir = Temp::new(name);
        let rootfs = dir.0.join("rootfs");
        base_image(&rootfs);
        let config = ScratchConfig::new(dir.0.join("scratch"), rootfs);
        Fixture { dir, config }
    }

    pub fn rootfs(&self) -> PathBuf {
        self.config.rootfs.clone()
    }

    pub fn manager(&self) -> ScratchManager {
        ScratchManager::new(self.config.clone()).unwrap()
    }
}

/// A failed test must not leave sandboxes running.
impl Drop for Fixture {
    fn drop(&mut self) {
        let Some(runsc) = &self.config.runsc else {
            return;
        };
        let runsc = |args: &[&str]| {
            let mut command = std::process::Command::new(runsc);
            command.arg(format!(
                "--root={}",
                self.config.root.join("runsc").display()
            ));
            if self.config.rootless {
                command.args(["--rootless", "--ignore-cgroups"]);
            }
            command.args(args).output()
        };
        let Ok(listed) = runsc(&["list", "-quiet"]) else {
            return;
        };
        for id in String::from_utf8_lossy(&listed.stdout).lines() {
            let _ = runsc(&["kill", id, "KILL"]);
            let _ = runsc(&["delete", "-force", id]);
        }
    }
}

/// A provider in memory: files with versions, and directories.
#[derive(Default)]
pub struct MemoryMount {
    files: Mutex<BTreeMap<String, (Vec<u8>, u64)>>,
    dirs: Mutex<BTreeSet<String>>,
    serial: AtomicU64,
    pub puts: AtomicU64,
}

impl MemoryMount {
    pub fn with(files: &[(&str, &str)]) -> Arc<MemoryMount> {
        let mount = MemoryMount::default();
        mount.dirs.lock().unwrap().insert("/".to_owned());
        for (path, body) in files {
            mount.set(path, body);
        }
        Arc::new(mount)
    }

    /// Changes a file behind scratch's back, as another editor would.
    pub fn set(&self, path: &str, body: &str) {
        let version = self.serial.fetch_add(1, Ordering::Relaxed) + 1;
        let mut dirs = self.dirs.lock().unwrap();
        let mut parent = path;
        while let Some(at) = parent.rfind('/') {
            parent = if at == 0 { "/" } else { &parent[..at] };
            dirs.insert(parent.to_owned());
            if parent == "/" {
                break;
            }
        }
        self.files
            .lock()
            .unwrap()
            .insert(path.to_owned(), (body.as_bytes().to_vec(), version));
    }

    pub fn get(&self, path: &str) -> Option<String> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .map(|(body, _)| String::from_utf8(body.clone()).unwrap())
    }

    fn entry(name: &str, kind: Kind, size: u64, version: u64) -> Entry {
        Entry {
            name: name.to_owned(),
            kind,
            size,
            mtime: 1_700_000_000,
            version: version.to_string(),
        }
    }

    fn name(path: &str) -> &str {
        path.rsplit('/').next().unwrap_or("")
    }
}

#[async_trait]
impl MountBackend for MemoryMount {
    async fn stat(&self, path: &str) -> Result<Entry, FsError> {
        if let Some((body, version)) = self.files.lock().unwrap().get(path) {
            return Ok(Self::entry(
                Self::name(path),
                Kind::File,
                body.len() as u64,
                *version,
            ));
        }
        if self.dirs.lock().unwrap().contains(path) {
            return Ok(Self::entry(Self::name(path), Kind::Dir, 0, 0));
        }
        Err(FsError::NotFound)
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<Entry>, FsError> {
        if !self.dirs.lock().unwrap().contains(path) {
            return Err(FsError::NotFound);
        }
        let prefix = if path == "/" {
            "/".to_owned()
        } else {
            format!("{path}/")
        };
        let direct = |candidate: &str| {
            candidate.starts_with(&prefix)
                && candidate != prefix
                && !candidate[prefix.len()..].contains('/')
        };
        let mut entries = Vec::new();
        for (file, (body, version)) in self.files.lock().unwrap().iter() {
            if direct(file) {
                entries.push(Self::entry(
                    Self::name(file),
                    Kind::File,
                    body.len() as u64,
                    *version,
                ));
            }
        }
        for dir in self.dirs.lock().unwrap().iter() {
            if direct(dir) {
                entries.push(Self::entry(Self::name(dir), Kind::Dir, 0, 0));
            }
        }
        Ok(entries)
    }

    async fn read(&self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>, FsError> {
        let files = self.files.lock().unwrap();
        let (body, _) = files.get(path).ok_or(FsError::NotFound)?;
        let start = (offset as usize).min(body.len());
        let end = (start + max as usize).min(body.len());
        Ok(body[start..end].to_vec())
    }

    async fn put(
        &self,
        path: &str,
        data: Vec<u8>,
        if_version: Option<String>,
        create_only: bool,
    ) -> Result<Entry, FsError> {
        self.puts.fetch_add(1, Ordering::Relaxed);
        if self.dirs.lock().unwrap().contains(path) {
            return Err(FsError::IsADirectory);
        }
        let mut files = self.files.lock().unwrap();
        if let Some((_, version)) = files.get(path) {
            if create_only {
                return Err(FsError::Exists);
            }
            if let Some(expected) = &if_version
                && *expected != version.to_string()
            {
                return Err(FsError::Conflict);
            }
        }
        let version = self.serial.fetch_add(1, Ordering::Relaxed) + 1;
        let size = data.len() as u64;
        files.insert(path.to_owned(), (data, version));
        Ok(Self::entry(Self::name(path), Kind::File, size, version))
    }

    async fn mkdir(&self, path: &str) -> Result<Entry, FsError> {
        if !self.dirs.lock().unwrap().insert(path.to_owned()) {
            return Err(FsError::Exists);
        }
        Ok(Self::entry(Self::name(path), Kind::Dir, 0, 0))
    }

    async fn rename(&self, src: &str, dst: &str) -> Result<(), FsError> {
        let mut files = self.files.lock().unwrap();
        let body = files.remove(src).ok_or(FsError::NotFound)?;
        files.insert(dst.to_owned(), body);
        Ok(())
    }

    async fn remove(&self, path: &str, _recursive: bool) -> Result<(), FsError> {
        if self.files.lock().unwrap().remove(path).is_some() {
            return Ok(());
        }
        if self.dirs.lock().unwrap().remove(path) {
            return Ok(());
        }
        Err(FsError::NotFound)
    }
}
