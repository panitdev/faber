//! Everything a target does that does not depend on how it is reached.
//!
//! This is the only implementation of [`Target`] in the crate. Every mode is a
//! `Machine` over a different pair of [`Spawn`] and [`Files`], and a mode is
//! therefore a fact about how one was *built* and about nothing else.
//!
//! That is the whole reason the mechanism traits exist. The agent is promised
//! that path shape, failure classes, exec semantics, truncation behavior, and
//! cwd handling are identical whatever it is talking to. With one
//! implementation per transport that promise holds only as long as four
//! authors keep agreeing; with the behavior here and only the plumbing below,
//! there is nothing left to disagree about.
//!
//! A transport that finds itself needing to special-case something in this
//! file is reporting a defect in the seam, not in itself.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::exec::{Chunk, Cursor, Exec, Exit, Outcome, ProcId, Signal, Stream};
use crate::fault::{Denial, Fault};
use crate::file::{
    Edit, Entry, EntryKind, LIST_CAP, Listing, Patch, PatchOp, Replace, Stat, Window,
};
use crate::files::Files;
use crate::manifest::{Capability, Manifest};
use crate::path::RootedPath;
use crate::spawn::{Proc, Run, Sink, Source, Spawn};
use crate::store::{Blob, Blobs, Span};
use crate::target::Target;

/// Bytes of one stream kept in the blob store. Past this the capture is
/// flagged truncated and the whole of it goes to a spill file.
pub const CAPTURE_CAP: usize = 256 * 1024;

/// Output this size or larger also lands at a path inside the target, because
/// a span cannot be `rg`'d and a path can.
pub const SPILL_THRESHOLD: usize = 64 * 1024;

/// Where spill files go, relative to the root.
///
/// Lifecycle cleanup is unresolved and this implementation does not pretend
/// otherwise: nothing deletes these. Faber owns no lifecycle, so a long run
/// leaves logs behind, under one predictable directory rather than scattered.
pub const SPILL_DIR: &str = "/.faber/logs";

/// A bound target: a manifest, a way to run things, and a way to move bytes.
pub struct Machine {
    /// `pub(crate)` so a constructor can assemble one and a test can take a
    /// capability away. Never mutated after bind by anything else — the
    /// manifest sits at a fixed position in the prefix and re-deriving it
    /// mid-run is prefix mutation.
    pub(crate) manifest: Manifest,
    spawn: Arc<dyn Spawn>,
    files: Arc<dyn Files>,
    blobs: Arc<dyn Blobs>,
    processes: Mutex<BTreeMap<ProcId, Background>>,
    next_id: AtomicU64,
    /// Distinguishes this binding's spill files from an earlier binding's over
    /// the same root. Call ids restart at 1 per bind, and nothing deletes a
    /// spill file — without this, a path recorded in an old transcript would
    /// later resolve to different bytes, which is the one property a spill
    /// path exists to provide.
    bind_nonce: u64,
}

/// A background process and everything read from it so far.
struct Background {
    proc: Box<dyn Proc>,
    stdin: Option<Sink>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    outcome: Option<Outcome>,
}

impl Machine {
    /// Assembles a bound target. Callers are the per-mode constructors, which
    /// own the probe and the manifest it produces.
    pub fn new(
        manifest: Manifest,
        spawn: Arc<dyn Spawn>,
        files: Arc<dyn Files>,
        blobs: Arc<dyn Blobs>,
    ) -> Self {
        let bind_nonce = manifest
            .probed_at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);

        Machine {
            manifest,
            spawn,
            files,
            blobs,
            processes: Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            bind_nonce,
        }
    }

    /// The cwd for a call: the request's, or the root. Per-call, never
    /// persistent.
    async fn resolve_cwd(&self, req: &Exec) -> Result<(RootedPath, String), Fault> {
        let rooted = req.cwd.clone().unwrap_or_else(|| self.root());
        let confined = self.files.confine(&rooted).await?;
        if confined.kind != Some(EntryKind::Dir) {
            return Err(Fault::Denied(Denial::NotFound {
                path: rooted.to_string(),
            }));
        }
        Ok((rooted, confined.path))
    }

    /// The argv and environment for a request. The shell comes from the
    /// manifest, never from the call.
    fn run(&self, req: &Exec, cwd: String) -> Run {
        let mut env: Vec<(String, String)> = self
            .manifest
            .agent_env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        // Additive overlay on the probed base, per-call and last.
        env.extend(req.env.iter().cloned());

        Run {
            argv: vec![
                self.manifest.shell.clone(),
                "-c".to_owned(),
                req.command.clone(),
            ],
            cwd,
            env,
            pty: false,
        }
    }

    /// Stores captured bytes, spilling to a path inside the target when they
    /// are large enough that the agent will want to grep rather than read,
    /// and flagging the capture when it was capped.
    async fn capture(&self, bytes: Vec<u8>, stream: &str, id: u64) -> Stream {
        let full = bytes.len();
        let spill = if full >= SPILL_THRESHOLD {
            self.spill(&bytes, stream, id).await
        } else {
            None
        };

        let kept = full.min(CAPTURE_CAP);
        let span = Span {
            blob: self.blobs.put(&bytes[..kept]),
            len: kept as u64,
            truncated: kept < full,
        };

        let mut captured = Stream::new(span);
        if let Some(path) = spill {
            captured = captured.spilled_to(path);
        }
        captured
    }

    async fn spill(&self, bytes: &[u8], stream: &str, id: u64) -> Option<RootedPath> {
        let path = RootedPath::new(
            &self.manifest.root,
            format!("{SPILL_DIR}/{:x}-{id}.{stream}", self.bind_nonce),
        )
        .ok()?;
        match self.files.store(&path, bytes).await {
            Ok(_) => Some(path),
            Err(error) => {
                tracing::warn!(%path, %error, "could not spill captured output");
                None
            }
        }
    }

    fn allocate(&self) -> ProcId {
        ProcId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    async fn apply_replace(&self, replace: &Replace) -> Result<Stat, Fault> {
        let bytes = self.files.fetch(&replace.path).await?;
        let text = String::from_utf8(bytes).map_err(|_| {
            Fault::Denied(Denial::Malformed {
                what: "edit".into(),
                reason: format!("`{}` is not valid utf-8", replace.path),
            })
        })?;

        let matches = text.matches(&replace.old).count();
        // Both refusals are deterministic, so they are denials rather than
        // failed writes: the agent must change the request, not retry it.
        if matches == 0 {
            return Err(Fault::Denied(Denial::EditRefused {
                path: replace.path.to_string(),
                reason: "the anchor text does not appear in the file".to_owned(),
            }));
        }
        if matches > 1 && !replace.all {
            return Err(Fault::Denied(Denial::EditRefused {
                path: replace.path.to_string(),
                reason: format!(
                    "the anchor text appears {matches} times; pass `all` to replace every occurrence"
                ),
            }));
        }

        let edited = if replace.all {
            text.replace(&replace.old, &replace.new)
        } else {
            text.replacen(&replace.old, &replace.new, 1)
        };
        self.files.store(&replace.path, edited.as_bytes()).await
    }

    /// Applies a patch set **sequentially, without atomicity**.
    ///
    /// Cross-file atomicity is left open because the transport may not support
    /// it. Nothing here provides it: a failing op leaves the earlier ones
    /// applied, and the returned [`Stat`] list says exactly which those were.
    async fn apply_patch(&self, patch: &Patch) -> Result<Vec<Stat>, Fault> {
        let mut stats = Vec::with_capacity(patch.ops.len());
        for op in &patch.ops {
            match op {
                PatchOp::Add { path, body } => stats.push(self.files.store(path, body).await?),
                PatchOp::Update(replace) => stats.push(self.apply_replace(replace).await?),
                PatchOp::Delete { path } => self.files.remove(path).await?,
                PatchOp::Move { from, to } => stats.push(self.files.rename(from, to).await?),
            }
        }
        Ok(stats)
    }

    /// Bytes accumulated past `from`, plus the cursor to resume at.
    async fn slice(&self, buffer: &Arc<Mutex<Vec<u8>>>, from: u64) -> (Span, u64) {
        let buffer = buffer.lock().await;
        let start = (from as usize).min(buffer.len());
        let end = (start + CAPTURE_CAP).min(buffer.len());
        let span = Span {
            blob: self.blobs.put(&buffer[start..end]),
            len: (end - start) as u64,
            truncated: end < buffer.len(),
        };
        (span, end as u64)
    }
}

#[async_trait]
impl Target for Machine {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    async fn exec(&self, req: Exec) -> Result<Exit, Fault> {
        self.manifest.require(Capability::Exec)?;
        let (cwd, real_cwd) = self.resolve_cwd(&req).await?;
        let id = self.allocate().0;

        let mut proc = self.spawn.spawn(self.run(&req, real_cwd)).await?;

        if let Some(mut pipe) = proc.stdin() {
            if let Some(body) = &req.stdin {
                let _ = pipe.write_all(body.as_bytes()).await;
            }
            let _ = pipe.shutdown().await;
        }

        let stdout = proc.stdout();
        let stderr = proc.stderr();
        let reading_stdout = tokio::spawn(async move { drain(stdout).await });
        let reading_stderr = tokio::spawn(async move { drain(stderr).await });

        let started = Instant::now();
        let outcome = match tokio::time::timeout(req.timeout, proc.wait()).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(fault)) => return Err(fault),
            Err(_) => {
                // The command ran; it just did not stop. Kill it, keep what it
                // produced, and report the outcome — not a fault.
                let _ = proc.signal(Signal::Kill).await;
                let _ = proc.wait().await;
                Outcome::TimedOut
            }
        };
        let wall = started.elapsed();

        let out = reading_stdout.await.unwrap_or_default();
        let err = reading_stderr.await.unwrap_or_default();

        Ok(Exit {
            target: self.manifest.label.clone(),
            cwd,
            outcome,
            stdout: self.capture(out, "stdout", id).await,
            stderr: self.capture(err, "stderr", id).await,
            wall,
        })
    }

    async fn start(&self, req: Exec) -> Result<ProcId, Fault> {
        self.manifest.require(Capability::Background)?;
        let (_, real_cwd) = self.resolve_cwd(&req).await?;

        let mut proc = self.spawn.spawn(self.run(&req, real_cwd)).await?;

        let mut stdin = proc.stdin();
        if let Some(body) = &req.stdin
            && let Some(pipe) = stdin.as_mut()
        {
            let _ = pipe.write_all(body.as_bytes()).await;
        }

        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        pump(proc.stdout(), Arc::clone(&stdout));
        pump(proc.stderr(), Arc::clone(&stderr));

        let id = self.allocate();
        self.processes.lock().await.insert(
            id,
            Background {
                proc,
                stdin,
                stdout,
                stderr,
                outcome: None,
            },
        );
        Ok(id)
    }

    async fn output(&self, id: ProcId, from: Cursor) -> Result<Chunk, Fault> {
        let mut processes = self.processes.lock().await;
        let process = processes
            .get_mut(&id)
            .ok_or(Fault::Denied(Denial::NoSuchProcess(id)))?;

        // Reap without blocking: a still-running process leaves `outcome`
        // `None`, which is what tells the reader to come back.
        if process.outcome.is_none() {
            process.outcome = process.proc.try_wait().await?;
        }

        let (stdout, next_stdout) = self.slice(&process.stdout, from.stdout).await;
        let (stderr, next_stderr) = self.slice(&process.stderr, from.stderr).await;

        Ok(Chunk {
            target: self.manifest.label.clone(),
            stdout,
            stderr,
            next: Cursor {
                stdout: next_stdout,
                stderr: next_stderr,
            },
            outcome: process.outcome,
        })
    }

    async fn stdin(&self, id: ProcId, body: &Blob) -> Result<(), Fault> {
        self.manifest.require(Capability::Stdin)?;
        let mut processes = self.processes.lock().await;
        let process = processes
            .get_mut(&id)
            .ok_or(Fault::Denied(Denial::NoSuchProcess(id)))?;

        let pipe = process
            .stdin
            .as_mut()
            .ok_or(Fault::Denied(Denial::Malformed {
                what: "stdin".into(),
                reason: "this process's stdin is already closed".to_owned(),
            }))?;

        pipe.write_all(body.as_bytes())
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        pipe.flush()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))
    }

    async fn signal(&self, id: ProcId, signal: Signal) -> Result<(), Fault> {
        let mut processes = self.processes.lock().await;
        let process = processes
            .get_mut(&id)
            .ok_or(Fault::Denied(Denial::NoSuchProcess(id)))?;

        // A reaped handle is spent, and its pid is free for the OS to hand to
        // someone else — signalling it would hit an unrelated process. The
        // denial is also the honest answer: there is nothing left to signal.
        if process.outcome.is_some() {
            return Err(Fault::Denied(Denial::NoSuchProcess(id)));
        }

        process.proc.signal(signal).await
    }

    async fn read(&self, path: &RootedPath, window: Option<Window>) -> Result<Span, Fault> {
        self.manifest.require(Capability::Read)?;
        let bytes = self.files.fetch(path).await?;

        let Some(window) = window else {
            let kept = bytes.len().min(CAPTURE_CAP);
            return Ok(Span {
                blob: self.blobs.put(&bytes[..kept]),
                len: kept as u64,
                truncated: kept < bytes.len(),
            });
        };

        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        // A window past the end is not an empty file.
        if window.offset > 0 && window.offset as usize >= lines.len() {
            return Err(Fault::Denied(Denial::OutOfRange {
                path: path.to_string(),
                offset: window.offset,
                len: lines.len() as u64,
            }));
        }

        let start = window.offset as usize;
        let end = (start + window.limit as usize).min(lines.len());
        let selected = lines[start..end].join("\n");

        Ok(Span {
            blob: self.blobs.put(selected.as_bytes()),
            len: selected.len() as u64,
            truncated: end < lines.len(),
        })
    }

    async fn write(&self, path: &RootedPath, body: &Blob) -> Result<Stat, Fault> {
        self.manifest.require(Capability::Write)?;
        self.files.store(path, body.as_bytes()).await
    }

    async fn edit(&self, edit: &Edit) -> Result<Vec<Stat>, Fault> {
        match edit {
            Edit::Replace(replace) => {
                self.manifest.require(Capability::Edit)?;
                Ok(vec![self.apply_replace(replace).await?])
            }
            Edit::Patch(patch) => {
                self.manifest.require(Capability::Patch)?;
                self.apply_patch(patch).await
            }
        }
    }

    async fn list(&self, path: &RootedPath, glob: Option<&str>) -> Result<Listing, Fault> {
        self.manifest.require(Capability::List)?;
        if let Some(pattern) = glob {
            // A rejected pattern is never reported as an empty result.
            validate_glob(pattern)?;
        }

        let found = self.files.enumerate(path).await?;

        let mut entries = Vec::new();
        let mut truncated = false;
        for entry in found {
            if let Some(pattern) = glob
                && !glob_matches(pattern, &entry.name)
            {
                continue;
            }
            if entries.len() >= LIST_CAP {
                // Cap *and* flag. A capped listing the model reads as complete
                // ends the search.
                truncated = true;
                break;
            }
            entries.push(Entry {
                path: path.join(&entry.name)?,
                kind: entry.kind,
                size: entry.size,
            });
        }

        entries.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        Ok(Listing { entries, truncated })
    }
}

async fn drain(reader: Option<Source>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut reader) = reader {
        let _ = reader.read_to_end(&mut bytes).await;
    }
    bytes
}

/// Reads a background stream into a buffer for [`Target::output`] to slice.
///
/// Unbounded on purpose-for-now: a background process producing gigabytes
/// grows this buffer without limit. Bounding it means deciding whether
/// background output spills the way [`Target::exec`]'s does, which is not
/// settled.
fn pump(reader: Option<Source>, into: Arc<Mutex<Vec<u8>>>) {
    let Some(mut reader) = reader else { return };
    tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => into.lock().await.extend_from_slice(&chunk[..read]),
            }
        }
    });
}

/// Glob evaluation is Faber-side: host-side is fast and image-dependent, and
/// image-dependent is the risk.
///
/// Non-recursive, so `*` does not cross a `/` — there is nothing to cross,
/// since [`Target::list`] matches against one directory's names.
pub(crate) fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    matches_from(&pattern, &name)
}

fn matches_from(pattern: &[char], name: &[char]) -> bool {
    match pattern.first() {
        None => name.is_empty(),
        Some('*') => (0..=name.len()).any(|split| matches_from(&pattern[1..], &name[split..])),
        Some('?') => !name.is_empty() && matches_from(&pattern[1..], &name[1..]),
        Some('[') => {
            let Some(close) = pattern.iter().position(|c| *c == ']') else {
                return false;
            };
            let (negated, set) = match pattern.get(1) {
                Some('!') => (true, &pattern[2..close]),
                _ => (false, &pattern[1..close]),
            };
            let Some(first) = name.first() else {
                return false;
            };
            (set.contains(first) != negated) && matches_from(&pattern[close + 1..], &name[1..])
        }
        Some(literal) => name.first() == Some(literal) && matches_from(&pattern[1..], &name[1..]),
    }
}

pub(crate) fn validate_glob(pattern: &str) -> Result<(), Fault> {
    if pattern.is_empty() {
        return Err(Fault::Denied(Denial::BadPattern {
            pattern: pattern.to_owned(),
            reason: "an empty pattern matches nothing; omit the glob instead".to_owned(),
        }));
    }
    let mut open = false;
    for character in pattern.chars() {
        match character {
            '[' if open => {
                return Err(Fault::Denied(Denial::BadPattern {
                    pattern: pattern.to_owned(),
                    reason: "nested `[`".to_owned(),
                }));
            }
            '[' => open = true,
            ']' => open = false,
            '/' => {
                return Err(Fault::Denied(Denial::BadPattern {
                    pattern: pattern.to_owned(),
                    reason: "`list` is non-recursive; a glob matches names in one directory"
                        .to_owned(),
                }));
            }
            _ => {}
        }
    }
    if open {
        return Err(Fault::Denied(Denial::BadPattern {
            pattern: pattern.to_owned(),
            reason: "unterminated `[`".to_owned(),
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_faber_side_without_crossing_a_directory() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(!glob_matches("*.rs", "main.md"));
        assert!(glob_matches("m?in.rs", "main.rs"));
        assert!(glob_matches("[mp]ain.rs", "main.rs"));
        assert!(!glob_matches("[!m]ain.rs", "main.rs"));
        assert!(validate_glob("src/*.rs").is_err());
    }
}
