//! Direct execution on this machine — the local, direct mode.
//!
//! The mode where the root boundary is load-bearing and unbacked: nothing
//! underneath enforces the root, so escape is refused here or not at all. Two
//! checks, because the lexical one in [`RootedPath`] cannot see a symlink:
//!
//! 1. [`RootedPath::new`] refuses `..`, `~`, and relative paths lexically.
//! 2. [`LocalFiles::confine`] canonicalizes and re-checks before every
//!    filesystem operation, which is what catches a symlink inside the root
//!    pointing out of it.
//!
//! What it does *not* catch: a shell command. `sh -c 'cat ../../etc/passwd'`
//! runs, because the process is the user's own and the API is not a sandbox.
//! That is precisely the posture this target publishes —
//! [`Posture::Conventional`] — and publishing it is the honest half of the
//! contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use tokio::fs;
use tokio::process::{Child, Command};

use crate::exec::{Outcome, Signal};
use crate::fault::{Denial, Fault};
use crate::file::{EntryKind, Stat};
use crate::files::{Confined, DirEntry, Files};
use crate::machine::Machine;
use crate::manifest::{Capability, Manifest, Posture, Reachability, Scope};
use crate::path::{Root, RootedPath};
use crate::probe::{SHELL, probe};
use crate::registry::Label;
use crate::spawn::{Proc, Run, Sink, Source, Spawn};
use crate::store::Blobs;

/// Direct execution on this machine.
///
/// A constructor, not a type: what it returns is a [`Machine`] like every
/// other mode's, differing only in the mechanism underneath it.
pub struct LocalTarget;

impl LocalTarget {
    /// Probes and binds. The probe happens once, here, and the manifest it
    /// produces is frozen for the life of the binding.
    pub async fn bind(
        label: impl Into<Label>,
        root: Root,
        blobs: Arc<dyn Blobs>,
    ) -> Result<Machine, Fault> {
        let label = label.into();
        let real_root = fs::canonicalize(root.as_str()).await.map_err(|error| {
            Fault::Denied(Denial::NotFound {
                path: format!("{root}: {error}"),
            })
        })?;

        // Through the same transport that will run everything else, so this
        // target describes itself the way every other mode does.
        let probed = probe(&LocalSpawn, real_root.to_string_lossy().into_owned()).await?;

        // No `Capability::Pty`: background stdin here is a pipe, so a program
        // that opens `/dev/tty` or checks `isatty` will not prompt into it.
        // Advertising it would hand the agent a capability it can only
        // disprove by a write that goes nowhere.
        let capabilities = BTreeSet::from([
            Capability::Exec,
            Capability::Background,
            Capability::Stdin,
            Capability::Read,
            Capability::Write,
            Capability::Edit,
            Capability::Patch,
            Capability::List,
        ]);

        // So rc files can see an agent is driving and skip fancy prompts.
        let agent_env = BTreeMap::from([
            ("FABER_AGENT".to_owned(), "1".to_owned()),
            ("FABER_TARGET".to_owned(), label.0.clone()),
        ]);

        let manifest = Manifest {
            label,
            os: probed.os,
            arch: probed.arch,
            shell: SHELL.to_owned(),
            root,
            tools: probed.tools,
            capabilities,
            scope: Scope::Workspace,
            // Nothing probed the network, and asserting either way gets
            // believed.
            network: Reachability::Unknown,
            // Direct on a host: this API refuses escape and nothing under it
            // does.
            posture: Posture::Conventional,
            agent_env,
            // Commands run through `sh -c`, not a login shell. Say so rather
            // than let an absent alias become a mystery.
            login_shell_sourced: false,
            probed_at: SystemTime::now(),
        };

        Ok(Machine::new(
            manifest,
            Arc::new(LocalSpawn),
            Arc::new(LocalFiles { real_root }),
            blobs,
        ))
    }
}

/// Processes started in this process's own machine.
pub struct LocalSpawn;

#[async_trait]
impl Spawn for LocalSpawn {
    async fn spawn(&self, run: Run) -> Result<Box<dyn Proc>, Fault> {
        if run.pty {
            return Err(Fault::Denied(Denial::MissingCapability(Capability::Pty)));
        }
        let Some((program, arguments)) = run.argv.split_first() else {
            return Err(Fault::Denied(Denial::Malformed {
                what: "command".into(),
                reason: "an empty argv names no program".to_owned(),
            }));
        };

        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir(&run.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &run.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|error| Fault::Unreachable(format!("could not spawn a shell: {error}")))?;

        Ok(Box::new(LocalProc {
            pid: child.id(),
            stdin: child.stdin.take().map(|pipe| Box::new(pipe) as Sink),
            stdout: child.stdout.take().map(|pipe| Box::new(pipe) as Source),
            stderr: child.stderr.take().map(|pipe| Box::new(pipe) as Source),
            child: Some(child),
            outcome: None,
        }))
    }
}

struct LocalProc {
    child: Option<Child>,
    stdin: Option<Sink>,
    stdout: Option<Source>,
    stderr: Option<Source>,
    pid: Option<u32>,
    outcome: Option<Outcome>,
}

impl LocalProc {
    /// Records a reaped status and releases the handle.
    fn reaped(&mut self, status: std::process::ExitStatus) -> Outcome {
        let outcome = outcome_of(status);
        self.outcome = Some(outcome);
        self.child = None;
        outcome
    }
}

#[async_trait]
impl Proc for LocalProc {
    fn stdin(&mut self) -> Option<Sink> {
        self.stdin.take()
    }

    fn stdout(&mut self) -> Option<Source> {
        self.stdout.take()
    }

    fn stderr(&mut self) -> Option<Source> {
        self.stderr.take()
    }

    async fn wait(&mut self) -> Result<Outcome, Fault> {
        if let Some(outcome) = self.outcome {
            return Ok(outcome);
        }
        let Some(child) = self.child.as_mut() else {
            return Err(Fault::Unreachable("the process handle is spent".to_owned()));
        };
        let status = child
            .wait()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        Ok(self.reaped(status))
    }

    async fn try_wait(&mut self) -> Result<Option<Outcome>, Fault> {
        if let Some(outcome) = self.outcome {
            return Ok(Some(outcome));
        }
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        match child.try_wait() {
            Ok(Some(status)) => Ok(Some(self.reaped(status))),
            Ok(None) => Ok(None),
            Err(error) => Err(Fault::Unreachable(error.to_string())),
        }
    }

    async fn signal(&mut self, signal: Signal) -> Result<(), Fault> {
        if self.outcome.is_some() || self.child.is_none() {
            return Err(Fault::Unreachable("the process handle is spent".to_owned()));
        }

        #[cfg(unix)]
        {
            let Some(pid) = self.pid else {
                return Err(Fault::Unreachable("the process has no pid".to_owned()));
            };
            // Safety: `kill(2)` with a pid this process spawned and has not
            // reaped. `kill_on_drop` keeps the handle alive until we do.
            let sent = unsafe { libc::kill(pid as libc::pid_t, signal.number()) };
            if sent != 0 {
                return Err(Fault::Unreachable(
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            Ok(())
        }

        #[cfg(not(unix))]
        {
            match self.child.as_mut() {
                Some(child) if matches!(signal, Signal::Kill | Signal::Term) => child
                    .start_kill()
                    .map_err(|error| Fault::Unreachable(error.to_string())),
                _ => Err(Fault::Denied(Denial::Malformed {
                    what: "signal".into(),
                    reason: "this platform can only terminate a process".to_owned(),
                })),
            }
        }
    }
}

/// This machine's own filesystem, reached through `tokio::fs`.
pub struct LocalFiles {
    /// The canonicalized root, for the symlink re-check.
    real_root: PathBuf,
}

impl LocalFiles {
    /// Resolves a rooted path to a real one, refusing anything that leaves the
    /// root after symlink resolution.
    ///
    /// The path need not exist — the deepest existing ancestor is
    /// canonicalized and the remainder appended — so `write` to a new file is
    /// confined by the same check as `read` of an existing one.
    async fn real(&self, path: &RootedPath) -> Result<(PathBuf, bool), Fault> {
        let mut existing = PathBuf::from(path.as_str());
        let mut trailing: Vec<std::ffi::OsString> = Vec::new();

        let real = loop {
            match fs::canonicalize(&existing).await {
                Ok(real) => break real,
                Err(_) => {
                    let Some(name) = existing.file_name().map(ToOwned::to_owned) else {
                        return Err(escape(
                            path,
                            "path does not resolve inside the target's root",
                        ));
                    };
                    trailing.push(name);
                    existing.pop();
                }
            }
        };

        let whole = trailing.is_empty();
        let mut resolved = real;
        for name in trailing.iter().rev() {
            resolved.push(name);
        }

        if !resolved.starts_with(&self.real_root) {
            return Err(escape(
                path,
                "resolves outside the target's root via a symlink",
            ));
        }
        Ok((resolved, whole))
    }
}

#[async_trait]
impl Files for LocalFiles {
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault> {
        let (resolved, exists) = self.real(path).await?;
        // Canonicalization already followed every symlink, so what is left is
        // the file it pointed at.
        let kind = if exists {
            match fs::metadata(&resolved).await {
                Ok(meta) if meta.is_dir() => Some(EntryKind::Dir),
                Ok(meta) if meta.is_file() => Some(EntryKind::File),
                Ok(_) => Some(EntryKind::Other),
                Err(_) => None,
            }
        } else {
            None
        };

        Ok(Confined {
            path: resolved.to_string_lossy().into_owned(),
            kind,
        })
    }

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        let (real, _) = self.real(path).await?;
        fs::read(&real).await.map_err(|error| missing(path, &error))
    }

    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        let (real, _) = self.real(path).await?;
        if let Some(parent) = real.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|error| Fault::Unreachable(error.to_string()))?;
        }
        fs::write(&real, body)
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        stat(path, &real).await
    }

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault> {
        let (real, _) = self.real(path).await?;
        fs::remove_file(&real)
            .await
            .map_err(|error| missing(path, &error))
    }

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault> {
        let (source, _) = self.real(from).await?;
        let (destination, _) = self.real(to).await?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|error| Fault::Unreachable(error.to_string()))?;
        }
        fs::rename(&source, &destination)
            .await
            .map_err(|error| missing(from, &error))?;
        stat(to, &destination).await
    }

    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault> {
        let (real, _) = self.real(dir).await?;
        let mut reading = fs::read_dir(&real)
            .await
            .map_err(|error| missing(dir, &error))?;

        let mut entries = Vec::new();
        while let Some(entry) = reading
            .next_entry()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?
        {
            let kind = match entry.file_type().await {
                Ok(kind) if kind.is_dir() => EntryKind::Dir,
                Ok(kind) if kind.is_file() => EntryKind::File,
                Ok(kind) if kind.is_symlink() => EntryKind::Symlink,
                Ok(_) => EntryKind::Other,
                Err(_) => EntryKind::Other,
            };
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind,
                size: entry.metadata().await.ok().map(|meta| meta.len()),
            });
        }
        Ok(entries)
    }
}

fn escape(path: &RootedPath, reason: &'static str) -> Fault {
    Fault::Denied(Denial::PathEscape {
        path: path.to_string(),
        reason: reason.into(),
    })
}

/// At the filesystem: a missing path is a denial naming the path, and
/// anything else is the transport's problem rather than the request's.
fn missing(path: &RootedPath, error: &std::io::Error) -> Fault {
    if error.kind() == std::io::ErrorKind::NotFound {
        Fault::Denied(Denial::NotFound {
            path: path.to_string(),
        })
    } else {
        Fault::Unreachable(error.to_string())
    }
}

async fn stat(path: &RootedPath, real: &Path) -> Result<Stat, Fault> {
    let meta = fs::metadata(real)
        .await
        .map_err(|error| missing(path, &error))?;
    Ok(Stat {
        path: path.clone(),
        size: meta.len(),
        modified: meta.modified().ok(),
    })
}

fn outcome_of(status: std::process::ExitStatus) -> Outcome {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(number) = status.signal()
            && let Some(signal) = Signal::from_number(number)
        {
            return Outcome::Signaled { signal };
        }
    }
    Outcome::Completed {
        code: status.code().unwrap_or(-1),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::exec::{Cursor, Exec, ProcId};
    use crate::file::{Edit, Patch, PatchOp, Replace, Window};
    use crate::machine::SPILL_DIR;
    use crate::store::{MemoryBlobs, Span};
    use crate::target::Target;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch root, and a target bound to it.
    pub(crate) fn scratch() -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("faber-env-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir).unwrap()
    }

    /// A target that skips the probe, for tests that only need a `dyn Target`.
    pub(crate) fn stub_target(label: &str) -> Machine {
        let dir = scratch();
        let root = Root::new(dir.to_string_lossy()).unwrap();
        Machine::new(
            Manifest {
                label: label.into(),
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                shell: SHELL.to_owned(),
                root,
                tools: BTreeMap::new(),
                capabilities: BTreeSet::from([
                    Capability::Exec,
                    Capability::Background,
                    Capability::Stdin,
                    Capability::Read,
                    Capability::Write,
                    Capability::Edit,
                    Capability::Patch,
                    Capability::List,
                ]),
                scope: Scope::Workspace,
                network: Reachability::Unknown,
                posture: Posture::Conventional,
                agent_env: BTreeMap::new(),
                login_shell_sourced: false,
                probed_at: SystemTime::now(),
            },
            Arc::new(LocalSpawn),
            Arc::new(LocalFiles { real_root: dir }),
            Arc::new(MemoryBlobs::new()),
        )
    }

    async fn target() -> (Machine, Arc<MemoryBlobs>) {
        let blobs = Arc::new(MemoryBlobs::new());
        let dir = scratch();
        let root = Root::new(dir.to_string_lossy()).unwrap();
        let target = LocalTarget::bind("work", root, blobs.clone() as Arc<dyn Blobs>)
            .await
            .unwrap();
        (target, blobs)
    }

    fn text(blobs: &MemoryBlobs, span: &Span) -> String {
        String::from_utf8(blobs.get(&span.blob).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_a_result_and_not_a_fault() {
        let (target, blobs) = target().await;
        let exit = target
            .exec(Exec::new("echo out; echo err >&2; exit 3"))
            .await
            .unwrap();

        assert_eq!(exit.outcome, Outcome::Completed { code: 3 });
        assert_eq!(text(&blobs, &exit.stdout.span).trim(), "out");
        assert_eq!(text(&blobs, &exit.stderr.span).trim(), "err");
    }

    #[tokio::test]
    async fn every_exit_echoes_its_target_and_resolved_cwd() {
        let (target, _) = target().await;
        let exit = target.exec(Exec::new("true")).await.unwrap();

        assert_eq!(exit.target.as_str(), "work");
        assert_eq!(exit.cwd.as_str(), target.root().as_str());
    }

    #[tokio::test]
    async fn a_timeout_is_an_outcome_rather_than_a_fault() {
        let (target, _) = target().await;
        let exit = target
            .exec(Exec::new("sleep 30").timeout(std::time::Duration::from_millis(150)))
            .await
            .unwrap();

        assert_eq!(exit.outcome, Outcome::TimedOut);
    }

    #[tokio::test]
    async fn cwd_does_not_carry_between_calls() {
        let (target, blobs) = target().await;
        target
            .write(&target.root().join("sub/marker").unwrap(), &"x".into())
            .await
            .unwrap();

        target
            .exec(Exec::new(&format!("cd {}/sub", target.root())))
            .await
            .unwrap();
        let exit = target.exec(Exec::new("pwd")).await.unwrap();

        assert_eq!(
            text(&blobs, &exit.stdout.span).trim(),
            target.root().as_str()
        );
    }

    #[tokio::test]
    async fn a_path_leaving_the_root_through_a_symlink_is_denied() {
        let (target, _) = target().await;
        let outside = scratch().join("secret");
        std::fs::write(&outside, "s3cret").unwrap();
        std::os::unix::fs::symlink(
            &outside,
            PathBuf::from(target.root().as_str()).join("link"),
        )
        .unwrap();

        let path = target.root().join("link").unwrap(); // lexically fine
        let fault = target.read(&path, None).await.unwrap_err();

        assert!(matches!(fault, Fault::Denied(Denial::PathEscape { .. })));
    }

    #[tokio::test]
    async fn reading_a_missing_file_is_not_an_empty_read() {
        let (target, _) = target().await;
        let fault = target
            .read(&target.root().join("nope.txt").unwrap(), None)
            .await
            .unwrap_err();

        assert!(matches!(fault, Fault::Denied(Denial::NotFound { .. })));
    }

    #[tokio::test]
    async fn a_window_past_the_end_is_out_of_range_and_a_window_inside_is_flagged() {
        let (target, blobs) = target().await;
        let path = target.root().join("lines.txt").unwrap();
        target.write(&path, &"a\nb\nc\nd\n".into()).await.unwrap();

        let span = target.read(&path, Some(Window::new(1, 2))).await.unwrap();
        assert_eq!(text(&blobs, &span), "b\nc");
        assert!(span.truncated);

        let fault = target
            .read(&path, Some(Window::new(99, 2)))
            .await
            .unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::OutOfRange { .. })));
    }

    #[tokio::test]
    async fn an_ambiguous_edit_is_refused_rather_than_guessed_at() {
        let (target, blobs) = target().await;
        let path = target.root().join("dup.txt").unwrap();
        target.write(&path, &"x\nx\n".into()).await.unwrap();

        let one = Edit::Replace(Replace::new(path.clone(), "x", "y"));
        let fault = target.edit(&one).await.unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::EditRefused { .. })));

        let all = Edit::Replace(Replace::new(path.clone(), "x", "y").all());
        target.edit(&all).await.unwrap();
        let span = target.read(&path, None).await.unwrap();
        assert_eq!(text(&blobs, &span), "y\ny\n");
    }

    #[tokio::test]
    async fn an_absent_anchor_is_a_denial_not_a_silent_no_op() {
        let (target, _) = target().await;
        let path = target.root().join("file.txt").unwrap();
        target.write(&path, &"hello\n".into()).await.unwrap();

        let edit = Edit::Replace(Replace::new(path, "goodbye", "hi"));
        assert!(matches!(
            target.edit(&edit).await.unwrap_err(),
            Fault::Denied(Denial::EditRefused { .. })
        ));
    }

    #[tokio::test]
    async fn a_patch_set_can_add_edit_move_and_delete() {
        let (target, blobs) = target().await;
        let first = target.root().join("a.txt").unwrap();
        let second = target.root().join("b.txt").unwrap();
        let moved = target.root().join("nested/c.txt").unwrap();

        target.write(&second, &"old\n".into()).await.unwrap();
        let patch = Patch::new(vec![
            PatchOp::Add {
                path: first.clone(),
                body: b"one\n".to_vec(),
            },
            PatchOp::Update(Replace::new(second.clone(), "old", "new")),
            PatchOp::Move {
                from: second.clone(),
                to: moved.clone(),
            },
        ]);

        let stats = target.edit(&Edit::Patch(patch)).await.unwrap();
        assert_eq!(stats.len(), 3);
        assert_eq!(
            text(&blobs, &target.read(&first, None).await.unwrap()),
            "one\n"
        );
        assert_eq!(
            text(&blobs, &target.read(&moved, None).await.unwrap()),
            "new\n"
        );
        assert!(matches!(
            target.read(&second, None).await.unwrap_err(),
            Fault::Denied(Denial::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn a_rejected_glob_is_never_reported_as_an_empty_listing() {
        let (target, _) = target().await;
        let root = target.root();

        let fault = target.list(&root, Some("[abc")).await.unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::BadPattern { .. })));

        let listing = target.list(&root, Some("*.rs")).await.unwrap();
        assert!(listing.entries.is_empty());
        assert!(!listing.truncated);
    }

    #[tokio::test]
    async fn a_listing_matches_faber_side_and_stays_in_one_directory() {
        let (target, _) = target().await;
        target
            .write(&target.root().join("keep.rs").unwrap(), &"".into())
            .await
            .unwrap();
        target
            .write(&target.root().join("skip.md").unwrap(), &"".into())
            .await
            .unwrap();
        target
            .write(&target.root().join("deep/also.rs").unwrap(), &"".into())
            .await
            .unwrap();

        let listing = target.list(&target.root(), Some("*.rs")).await.unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, vec!["/keep.rs"]);
    }

    #[tokio::test]
    async fn listing_a_missing_directory_is_not_found() {
        let (target, _) = target().await;
        let fault = target
            .list(&target.root().join("nowhere").unwrap(), None)
            .await
            .unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::NotFound { .. })));
    }

    #[tokio::test]
    async fn a_background_process_reads_forward_from_a_cursor_and_takes_stdin() {
        let (target, blobs) = target().await;
        let id = target
            .start(Exec::new("while read line; do echo \"got $line\"; done"))
            .await
            .unwrap();

        target.stdin(id, &"one\n".into()).await.unwrap();
        let mut cursor = Cursor::START;
        let first = loop {
            let chunk = target.output(id, cursor).await.unwrap();
            cursor = chunk.next;
            if chunk.stdout.len > 0 {
                break text(&blobs, &chunk.stdout);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        assert_eq!(first.trim(), "got one");

        target.signal(id, Signal::Term).await.unwrap();
    }

    #[tokio::test]
    async fn a_spent_handle_is_not_signalled() {
        // The pid is free for the OS to reuse the moment the child is reaped,
        // so signalling a finished process would reach someone else's.
        let (target, _) = target().await;
        let id = target.start(Exec::new("true")).await.unwrap();

        loop {
            if target
                .output(id, Cursor::START)
                .await
                .unwrap()
                .outcome
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let fault = target.signal(id, Signal::Term).await.unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::NoSuchProcess(_))));
    }

    #[tokio::test]
    async fn an_unknown_process_handle_is_denied_rather_than_unreachable() {
        let (target, _) = target().await;
        let fault = target.output(ProcId(999), Cursor::START).await.unwrap_err();
        assert!(matches!(fault, Fault::Denied(Denial::NoSuchProcess(_))));
    }

    #[tokio::test]
    async fn large_output_spills_to_a_path_inside_the_target() {
        let (target, _) = target().await;
        let exit = target
            .exec(Exec::new("head -c 200000 /dev/zero | tr '\\0' 'a'"))
            .await
            .unwrap();

        let spill = exit.stdout.spill.expect("large output spills");
        assert!(spill.as_str().starts_with(SPILL_DIR));
        let span = target.read(&spill, None).await.unwrap();
        assert_eq!(span.len, 200_000);
    }

    #[tokio::test]
    async fn a_missing_capability_is_denied_by_name() {
        let (mut target, _) = target().await;
        target.manifest.capabilities.remove(&Capability::Write);

        let fault = target
            .write(&target.root().join("x").unwrap(), &"x".into())
            .await
            .unwrap_err();
        assert!(matches!(
            fault,
            Fault::Denied(Denial::MissingCapability(Capability::Write))
        ));
    }
}
