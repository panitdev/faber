//! Running a command inside a container.
//!
//! Two things here are not obvious from the Engine API's shape.
//!
//! **Output arrives multiplexed.** A non-TTY exec sends stdout and stderr down
//! one hijacked connection, each payload behind an eight-byte header naming
//! which stream it belongs to. Faber's contract keeps them separate, so a task
//! demultiplexes into two pipes and the rest of the crate never learns that
//! they arrived together.
//!
//! **There is no signal endpoint.** `docker kill` signals the *container*,
//! which is container lifecycle and not ours to touch — killing a container to
//! interrupt one command would destroy state a user may be mid-way through
//! inspecting. So the command records its own pid on the way in, and a signal
//! is a second exec running `kill`. The pid is the one inside the container's
//! namespace, which is the only one that is meaningful from inside it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::docker::daemon::Daemon;
use crate::docker::engine;
use crate::exec::{Outcome, Signal};
use crate::fault::{Denial, Fault};
use crate::spawn::{Proc, Run, Sink, Source, Spawn};

/// How long `wait` sleeps between asking the daemon whether an exec has ended.
///
/// The Engine API has no blocking wait for an exec, so this is a poll. Short
/// enough that a fast command is not held up noticeably, long enough that a
/// slow one is not a thousand round trips.
const POLL: Duration = Duration::from_millis(50);

/// Where a command's pid is left so a later signal can find it.
///
/// Under the same directory as spilled output, and with the same unresolved
/// question hanging over it: nothing deletes these. One small file per command
/// is the cost of being able to interrupt one. Tiny per command and unbounded
/// over a long session — whatever eventually reaps them fixes a slow leak.
const PROC_DIR: &str = ".faber/proc";

/// Processes started inside a container.
pub struct DockerSpawn {
    daemon: Arc<dyn Daemon>,
    container: String,
    /// The container-side root, so pid files land somewhere known.
    root: String,
    nonce: u64,
    next: AtomicU64,
}

impl DockerSpawn {
    pub fn new(
        daemon: Arc<dyn Daemon>,
        container: impl Into<String>,
        root: impl Into<String>,
    ) -> Self {
        DockerSpawn {
            daemon,
            container: container.into(),
            root: root.into(),
            nonce: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos() as u64),
            next: AtomicU64::new(1),
        }
    }

    fn pid_path(&self) -> String {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let root = self.root.trim_end_matches('/');
        format!("{root}/{PROC_DIR}/{:x}-{id}", self.nonce)
    }
}

#[async_trait]
impl Spawn for DockerSpawn {
    async fn spawn(&self, run: Run) -> Result<Box<dyn Proc>, Fault> {
        let Some((program, arguments)) = run.argv.split_first() else {
            return Err(Fault::Denied(Denial::Malformed {
                what: "command".into(),
                reason: "an empty argv names no program".to_owned(),
            }));
        };

        // `exec "$0" "$@"` runs the real command *as this same process*, so
        // the pid just recorded is the command's own and not a parent's. It
        // also means the command is passed as arguments rather than
        // interpolated into the script, so nothing here has to be quoted
        // correctly to be safe.
        let pid_path = self.pid_path();
        let preamble = format!(
            "mkdir -p '{}' 2>/dev/null; printf %s $$ > '{}' 2>/dev/null; exec \"$0\" \"$@\"",
            parent_of(&pid_path),
            pid_path,
        );

        let mut cmd = vec!["/bin/sh".to_owned(), "-c".to_owned(), preamble];
        cmd.push(program.clone());
        cmd.extend(arguments.iter().cloned());

        let exec = engine::exec_create(
            &self.daemon,
            &self.container,
            &cmd,
            &run.env,
            &run.cwd,
            run.pty,
        )
        .await?;
        let stream = engine::exec_start(&self.daemon, &exec, run.pty).await?;

        let (writer, stdin) = tokio::io::split(stream);
        let (stdout, stderr) = demultiplex(writer, run.pty);

        Ok(Box::new(DockerProc {
            daemon: Arc::clone(&self.daemon),
            container: self.container.clone(),
            exec,
            pid_path,
            stdin: Some(Box::new(stdin) as Sink),
            stdout: Some(stdout),
            stderr: Some(stderr),
            outcome: None,
        }))
    }
}

/// Splits the hijacked stream into the two the contract promises.
///
/// A TTY exec is not framed — the pty already merged the streams — so its
/// output is all stdout and stderr stays empty. Saying so is better than
/// inventing a split that the terminal already destroyed.
fn demultiplex<R>(mut stream: R, tty: bool) -> (Source, Source)
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    let (out_writer, out_reader) = tokio::io::duplex(64 * 1024);
    let (err_writer, err_reader) = tokio::io::duplex(64 * 1024);

    tokio::spawn(async move {
        let mut out = out_writer;
        let mut err = err_writer;

        if tty {
            let _ = tokio::io::copy(&mut stream, &mut out).await;
            return;
        }

        loop {
            let mut header = [0u8; 8];
            if stream.read_exact(&mut header).await.is_err() {
                break;
            }
            let size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
            let mut payload = vec![0u8; size];
            if stream.read_exact(&mut payload).await.is_err() {
                break;
            }
            // 1 is stdout and 2 is stderr; 0 is stdin echoed back, which a
            // reader of output never wants.
            let sank = match header[0] {
                1 => out.write_all(&payload).await,
                2 => err.write_all(&payload).await,
                _ => Ok(()),
            };
            if sank.is_err() {
                break;
            }
        }
    });

    (Box::new(out_reader), Box::new(err_reader))
}

struct DockerProc {
    daemon: Arc<dyn Daemon>,
    container: String,
    exec: String,
    pid_path: String,
    stdin: Option<Sink>,
    stdout: Option<Source>,
    stderr: Option<Source>,
    outcome: Option<Outcome>,
}

impl DockerProc {
    /// Turns a finished exec's state into an outcome.
    ///
    /// A process the daemon reports as killed by a signal still surfaces as an
    /// exit code here, because the Engine API does not distinguish them — 137
    /// is what a `SIGKILL`ed process looks like from outside, and inventing
    /// `Signaled` from that would be a guess dressed as an observation.
    fn ended(&mut self, state: engine::ExecState) -> Outcome {
        let outcome = Outcome::Completed {
            code: state.exit_code.unwrap_or(-1) as i32,
        };
        self.outcome = Some(outcome);
        outcome
    }
}

/// The pid a command recorded for itself, inside the container.
///
/// Free rather than a method: reading it borrows the daemon and two strings,
/// all of which are shareable, where borrowing the whole process would drag
/// its pipes — which are not — into the future's captures.
async fn recorded_pid(daemon: &Arc<dyn Daemon>, container: &str, pid_path: &str) -> Option<u32> {
    let tar = engine::archive_get(daemon, container, pid_path)
        .await
        .ok()?;
    let bytes = super::files::single_file(&tar).ok()?;
    String::from_utf8_lossy(&bytes).trim().parse().ok()
}

#[async_trait]
impl Proc for DockerProc {
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
        loop {
            let state = engine::exec_inspect(&self.daemon, &self.exec).await?;
            if !state.running {
                return Ok(self.ended(state));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn try_wait(&mut self) -> Result<Option<Outcome>, Fault> {
        if let Some(outcome) = self.outcome {
            return Ok(Some(outcome));
        }
        let state = engine::exec_inspect(&self.daemon, &self.exec).await?;
        if state.running {
            return Ok(None);
        }
        Ok(Some(self.ended(state)))
    }

    async fn signal(&mut self, signal: Signal) -> Result<(), Fault> {
        let Some(pid) = recorded_pid(&self.daemon, &self.container, &self.pid_path).await else {
            return Err(Fault::Unreachable(
                "this command did not record a pid, so it cannot be signalled".to_owned(),
            ));
        };

        // `kill` is a shell builtin, so this does not depend on the image
        // carrying one. The exit code is the whole answer; nothing parses its
        // output.
        let cmd = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("kill -{} {pid}", signal.number()),
        ];
        let exec =
            engine::exec_create(&self.daemon, &self.container, &cmd, &[], "/", false).await?;
        let stream = engine::exec_start(&self.daemon, &exec, false).await?;
        drop(stream);

        Ok(())
    }
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(at) => path[..at].to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pid_file_lands_under_the_root_it_was_given() {
        let spawn = DockerSpawn::new(
            Arc::new(crate::docker::daemon::LocalSocket::new("unix:///nowhere").unwrap()),
            "build",
            "/work",
        );
        let path = spawn.pid_path();
        assert!(path.starts_with("/work/.faber/proc/"), "{path}");
        assert_eq!(parent_of(&path), "/work/.faber/proc");
    }

    #[test]
    fn a_root_of_slash_does_not_produce_a_doubled_separator() {
        let spawn = DockerSpawn::new(
            Arc::new(crate::docker::daemon::LocalSocket::new("unix:///nowhere").unwrap()),
            "build",
            "/",
        );
        assert!(spawn.pid_path().starts_with("/.faber/proc/"));
    }
}
