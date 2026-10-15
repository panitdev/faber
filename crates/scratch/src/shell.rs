//! The virtual shell: bashkit over scratch's filesystem.
//!
//! Each run is a new shell, as each `exec` is on every other environment.
//! Nothing persists between runs except what the command wrote to the
//! filesystem.
//!
//! A run that fails is the answer: whether a command runs here or in the
//! sandbox is decided before it runs ([`crate::decide`]), and nothing is run
//! a second time. Errors are worded the way coreutils words them where
//! bashkit's own wording differs ([`coreutils_wording`]).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bashkit::{Bash, ExecOptions, ExecutionLimits, FileSystem};
use environment::{Outcome, Signal};
use tokio::sync::watch;

use crate::vfs::ScratchFs;

/// Bytes of either stream the virtual shell keeps before truncating. The
/// capture above it applies its own, smaller cap; this one only has to be
/// large enough never to be the one that bites.
const STREAM_LIMIT: usize = 64 << 20;

/// How long a cancelled run may take to notice before it is dropped.
const CANCEL_GRACE: Duration = Duration::from_millis(200);

/// What one virtual run produced.
#[derive(Debug)]
pub struct VirtualRun {
    pub outcome: Outcome,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A stream with bashkit's framing of a filesystem error taken out, so a
/// failed virtual run reads as the real tool's would:
/// `cat: /nope: io error: No such file or directory` becomes
/// `cat: /nope: No such file or directory`. Only lines shaped like a tool's
/// error — a command name, then `: ` — are touched, since some tools (`grep`)
/// put theirs on stdout among the data.
pub fn coreutils_wording(stream: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(stream) else {
        return stream.to_vec();
    };
    if !text.contains(": io error: ") {
        return stream.to_vec();
    }
    text.split_inclusive('\n')
        .map(|line| {
            let tool = line.split_once(": ").map(|(tool, _)| tool);
            if tool.is_some_and(|tool| !tool.is_empty() && !tool.contains(char::is_whitespace)) {
                line.replacen(": io error: ", ": ", 1)
            } else {
                line.to_owned()
            }
        })
        .collect::<String>()
        .into_bytes()
}

/// One command to run virtually.
pub struct VirtualCommand<'a> {
    pub script: &'a str,
    pub cwd: &'a str,
    pub env: &'a [(String, String)],
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
}

/// Runs a command in the virtual shell. `cancel` stops it early; the run
/// then reports [`Outcome::Signaled`] with what it had produced.
pub async fn run(
    fs: Arc<ScratchFs>,
    command: VirtualCommand<'_>,
    mut cancel: watch::Receiver<bool>,
) -> VirtualRun {
    let limits = ExecutionLimits::new()
        .timeout(command.timeout)
        .max_commands(1_000_000)
        .max_loop_iterations(10_000_000)
        .max_total_loop_iterations(100_000_000)
        .max_input_bytes(STREAM_LIMIT)
        .max_stdout_bytes(STREAM_LIMIT)
        .max_stderr_bytes(STREAM_LIMIT)
        .max_aggregate_input_bytes(1 << 30)
        .max_live_intermediate_bytes(STREAM_LIMIT as u64)
        .max_work_units(u64::MAX / 2);
    let mut builder = Bash::builder()
        .fs(fs as Arc<dyn FileSystem>)
        .cwd(PathBuf::from(command.cwd))
        .limits(limits);
    for (key, value) in command.env {
        builder = builder.env(key, value);
    }
    let mut bash = builder.build();
    let token = bash.cancellation_token();

    let mut options = ExecOptions::new();
    if let Some(stdin) = command.stdin {
        options = options.stdin(stdin);
    }
    let script = command.script.to_owned();
    let running = bash.exec_with_options(&script, options);
    tokio::pin!(running);

    let mut cancelled = false;
    let result = loop {
        tokio::select! {
            result = &mut running => break result,
            changed = cancel.changed(), if !cancelled => {
                if changed.is_err() || *cancel.borrow() {
                    cancelled = true;
                    token.store(true, Ordering::Relaxed);
                }
            }
            // A builtin blocked in a wait (`sleep`, `read`) only sees the
            // token when it returns; past a short grace the run is dropped
            // where it stands, as a killed process would be.
            () = tokio::time::sleep(CANCEL_GRACE), if cancelled => {
                break Err(bashkit::Error::Cancelled);
            }
        }
    };

    match result {
        Ok(result) if !cancelled => VirtualRun {
            outcome: Outcome::Completed {
                code: result.exit_code,
            },
            stdout: coreutils_wording(result.stdout.as_bytes()),
            stderr: coreutils_wording(result.stderr.as_bytes()),
        },
        Ok(result) => VirtualRun {
            outcome: Outcome::Signaled {
                signal: Signal::Kill,
            },
            stdout: coreutils_wording(result.stdout.as_bytes()),
            stderr: coreutils_wording(result.stderr.as_bytes()),
        },
        Err(bashkit::Error::Cancelled) => VirtualRun {
            outcome: Outcome::Signaled {
                signal: Signal::Kill,
            },
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
        Err(error) => {
            let timed_out = matches!(
                &error,
                bashkit::Error::ResourceLimit(bashkit::LimitExceeded::Timeout(_))
            );
            VirtualRun {
                outcome: if timed_out {
                    Outcome::TimedOut
                } else {
                    Outcome::Completed { code: 1 }
                },
                stdout: Vec::new(),
                stderr: format!(
                    "scratch: the virtual shell stopped: {error}; re-run with `compute: true`\n"
                )
                .into_bytes(),
            }
        }
    }
}
