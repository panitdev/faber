//! Turning what an environment answered into what the model reads.
//!
//! This is the projection, and it is where the environment layer's guarantees
//! either survive or quietly disappear. Three of them are easy to lose here and
//! are therefore written out at every site:
//!
//! - **A nonzero exit is a result.** It is rendered as an outcome line, never
//!   as an error, and `is_error` is reserved for the two failure classes.
//! - **Truncation is always flagged.** A capped result the model reads as
//!   complete is worse than no result: it ends the search with a wrong answer
//!   instead of an incomplete one.
//! - **Every result echoes `target:path`.** No unqualified path enters the
//!   transcript, because `/src/main.rs` denotes a different file per target and
//!   a transcript that does not say which one cannot be replayed or reviewed.

use std::fmt::Write as _;

use environment::{Blobs, Chunk, Denial, Exit, Fault, Manifest, Outcome, Span, Stat};

/// A failure, said plainly: what happened, then what to do about it.
///
/// A failure the call caused says what to change; one it did not cause says
/// that changing the call will not help. Without that second half a model
/// debugs its shell script when the tunnel dropped.
pub fn fault(fault: &Fault) -> String {
    match fault {
        Fault::Unreachable(reason) => format!(
            "Could not reach the environment: {reason}\n\
             The call may or may not have taken effect. Your call did not cause this, so \
             changing it will not help. Try again later, or tell the user."
        ),
        Fault::Denied(denial) => denied(denial),
    }
}

fn denied(denial: &Denial) -> String {
    match denial {
        Denial::PathEscape { path, reason } => {
            format!("`{path}` cannot be used: {reason}. Give a path inside this environment.")
        }
        Denial::MissingCapability(capability) => format!(
            "This environment does not support {capability}. Use another environment for \
             this, or tell the user it cannot be done here."
        ),
        Denial::Disabled => "This environment is turned off, and only the user can turn it \
             back on. Tell the user."
            .to_owned(),
        Denial::NotBound { label } => format!(
            "There is no environment named `{label}`. Use one of the environment names you \
             were given."
        ),
        Denial::NotFound { path } => {
            format!("`{path}` does not exist. Check the path, or list its parent directory.")
        }
        Denial::OutOfRange { path, offset, len } => format!(
            "Offset {offset} is past the end of `{path}`, which has {len}. Use a smaller offset."
        ),
        Denial::BadPattern { pattern, reason } => {
            format!("`{pattern}` is not a valid pattern: {reason}. Fix the pattern and try again.")
        }
        Denial::EditRefused { path, reason } => format!(
            "Could not edit `{path}`: {reason}. Read the file and make the edit match what \
             it contains now."
        ),
        Denial::NoSuchProcess(id) => format!(
            "There is no process {id}. It may have ended already. List the processes to see \
             which ones exist."
        ),
        Denial::Stopped(id) => format!(
            "Process {id} was stopped because its environment went idle. Start it again if \
             you still need it."
        ),
        // Scratch refusing for its own limits, such as a quota: the call is
        // fine and no version of it will get through.
        Denial::Malformed { what, reason } if what == "scratch" => format!(
            "Scratch cannot do this right now: {reason}\n\
             Changing the call will not help. Tell the user."
        ),
        Denial::Malformed { what, reason } => format!(
            "{what}: {reason}\n\
             The call was not carried out. Fix what this says before trying again."
        ),
    }
}

/// A finished command.
pub fn exit(exit: &Exit, blobs: &dyn Blobs) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}:{} — {}",
        exit.target,
        exit.cwd,
        outcome(&exit.outcome)
    );
    let _ = writeln!(out, "ran for {:.2}s", exit.wall.as_secs_f64());

    for (name, stream) in [("stdout", &exit.stdout), ("stderr", &exit.stderr)] {
        out.push('\n');
        let _ = write!(out, "{name}: ");
        out.push_str(&captured(&stream.span, blobs));
        if let Some(path) = &stream.spill {
            let _ = write!(
                out,
                "\nthe whole of {name} is also at {}:{path} — grep it rather than \
                 re-reading it here",
                exit.target
            );
        }
    }
    out
}

/// How a command ended, in the words the model has to act on.
fn outcome(outcome: &Outcome) -> String {
    match outcome {
        // Stated as a result, because it is one: the command ran and reported.
        Outcome::Completed { code: 0 } => "exit 0".to_owned(),
        Outcome::Completed { code } => {
            format!(
                "exit {code} (the command ran and reported this; it is a result, not a failure)"
            )
        }
        Outcome::TimedOut => "timed out and was killed (it ran; it did not stop. What it \
                              printed before then is below)"
            .to_owned(),
        Outcome::Signaled { signal } => format!("killed by {signal}"),
    }
}

/// Output from a background process, plus where to resume.
pub fn chunk(chunk: &Chunk, blobs: &dyn Blobs) -> String {
    let mut out = String::new();
    let state = match &chunk.outcome {
        None => "still running".to_owned(),
        Some(ended) => outcome(ended),
    };
    let _ = writeln!(out, "{} — {state}", chunk.target);
    let _ = writeln!(
        out,
        "resume from stdout={} stderr={}",
        chunk.next.stdout, chunk.next.stderr
    );

    for (name, span) in [("stdout", &chunk.stdout), ("stderr", &chunk.stderr)] {
        out.push('\n');
        let _ = write!(out, "{name}: ");
        out.push_str(&captured(span, blobs));
    }
    out
}

/// Stored bytes, redeemed into text, with the flag a capped capture must carry.
fn captured(span: &Span, blobs: &dyn Blobs) -> String {
    if span.len == 0 && !span.truncated {
        // Distinct from "not read": the stream ran and produced nothing.
        return "empty".to_owned();
    }

    let Some(bytes) = blobs.get(&span.blob) else {
        return format!(
            "{} bytes were captured but could not be read back from storage",
            span.len
        );
    };

    let mut out = format!("{} bytes", span.len);
    if span.truncated {
        out.push_str(" — TRUNCATED, there was more than this");
    }
    out.push('\n');
    out.push_str(&String::from_utf8_lossy(&bytes));
    out
}

/// A file's contents.
pub fn read(target: &str, path: &str, span: &Span, blobs: &dyn Blobs) -> String {
    let mut out = format!("{target}:{path}\n");
    out.push_str(&captured(span, blobs));
    if span.truncated {
        out.push_str("\n(read a window with `offset` and `limit` to see the rest)");
    }
    out
}

/// What one operation left behind.
fn stat(target: &str, stat: &Stat) -> String {
    format!("{target}:{} — {} bytes", stat.path, stat.size)
}

/// What a patch left behind, in the order the operations ran.
///
/// Named as "applied so far" on purpose: the ops are not atomic, and a caller
/// reading this after a failure needs to know it is looking at the prefix that
/// survived rather than at the whole request.
pub fn stats(target: &str, applied: &[Stat]) -> String {
    if applied.is_empty() {
        return format!("{target}: nothing was written");
    }
    let mut out = format!("{} operations applied, in order:\n", applied.len());
    for entry in applied {
        let _ = writeln!(out, "  {}", stat(target, entry));
    }
    out
}

/// One target's manifest.
pub struct Target<'a> {
    pub manifest: &'a Manifest,
}

/// Every bound environment at once.
///
/// All of them together, deliberately. The differences between environments
/// are the thing the model most needs to reason about — one has `cargo`, the
/// other has a network — and it cannot notice a difference it is shown one
/// side of at a time.
pub fn manifests(targets: &[Target<'_>]) -> String {
    let manifests: Vec<&Manifest> = targets.iter().map(|target| target.manifest).collect();
    if manifests.is_empty() {
        return "No environments are bound to this session. Nothing here can run \
                anywhere until one is added, which is the user's to do — there is no \
                verb that binds one."
            .to_owned();
    }

    let mut out = String::new();
    for target in targets {
        let manifest = target.manifest;
        let _ = writeln!(out, "{}", manifest.label);
        let _ = writeln!(out, "  root      {}", manifest.root);
        let _ = writeln!(out, "  system    {} {}", manifest.os, manifest.arch);
        let _ = writeln!(out, "  shell     {}", manifest.shell);
        let _ = writeln!(
            out,
            "  scope     {}",
            match manifest.scope {
                environment::Scope::Container => "the container's filesystem",
                environment::Scope::Workspace => "a directory on a machine carrying more",
            }
        );
        let _ = writeln!(
            out,
            "  root is   {}",
            match manifest.posture {
                environment::Posture::Enforced =>
                    "enforced by the container: a command cannot walk out of it",
                environment::Posture::Conventional =>
                    "held by this API alone: a shell command that walks out of it is not \
                     stopped, so stay inside it deliberately",
            }
        );
        let _ = writeln!(
            out,
            "  network   {}",
            match manifest.network {
                environment::Reachability::Reachable => "reachable",
                environment::Reachability::Unreachable => "not reachable",
                environment::Reachability::Unknown => "unknown — nothing measured it",
            }
        );
        let _ = writeln!(
            out,
            "  answers   {}",
            manifest
                .capabilities
                .iter()
                .map(|capability| format!("{capability}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let tools = if manifest.tools.is_empty() {
            "none of the ones probed for".to_owned()
        } else {
            manifest
                .tools
                .iter()
                .map(|(name, version)| format!("{name} {version}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(out, "  found     {tools}");
        if !manifest.login_shell_sourced {
            let _ = writeln!(
                out,
                "  note      login shell files were not sourced, so aliases and functions \
                 from them are not available"
            );
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use environment::{BlobRef, MemoryBlobs, Span};

    use super::*;

    #[test]
    fn a_nonzero_exit_is_rendered_as_a_result() {
        let text = outcome(&Outcome::Completed { code: 1 });
        assert!(text.contains("exit 1"));
        assert!(text.contains("not a failure"));
    }

    #[test]
    fn a_capped_capture_says_so_where_the_bytes_are() {
        let blobs = MemoryBlobs::new();
        let blob = blobs.put(b"half of it");
        let span = Span {
            blob,
            len: 10,
            truncated: true,
        };
        let text = captured(&span, &blobs);
        assert!(text.contains("TRUNCATED"));
        assert!(text.contains("half of it"));
    }

    #[test]
    fn an_empty_stream_is_not_a_missing_one() {
        let blobs = MemoryBlobs::new();
        let empty = Span {
            blob: blobs.put(b""),
            len: 0,
            truncated: false,
        };
        assert_eq!(captured(&empty, &blobs), "empty");

        // Bytes that were captured and then lost are a third thing again, and
        // must never render as "empty" — that would be a settled negative
        // answer to a question nothing actually answered.
        let lost = Span {
            blob: BlobRef("mem:404".into()),
            len: 12,
            truncated: false,
        };
        assert!(captured(&lost, &blobs).contains("could not be read back"));
    }

    #[test]
    fn a_failure_says_what_happened_and_whether_changing_the_call_helps() {
        let denied = fault(&Fault::Denied(Denial::BadPattern {
            pattern: "src/*".into(),
            reason: "non-recursive".into(),
        }));
        assert_eq!(
            denied,
            "`src/*` is not a valid pattern: non-recursive. Fix the pattern and try again."
        );

        let gone = fault(&Fault::Unreachable("connection reset".into()));
        assert!(gone.starts_with("Could not reach the environment: connection reset"));
        assert!(gone.contains("changing it will not help"));
    }

    #[test]
    fn a_quota_refusal_does_not_ask_for_a_different_call() {
        let quota = fault(&Fault::Denied(Denial::Malformed {
            what: "scratch".into(),
            reason: "over quota".into(),
        }));
        assert!(quota.contains("Changing the call will not help"), "{quota}");
    }
}
