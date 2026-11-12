//! `exec`, `process`, `read` and `patch`: the declared tools, their schemas,
//! and what each call does once it knows where it runs.
//!
//! `execute_in` is required on every tool and never defaulted, and every
//! path and `cwd` is absolute: a relative one is refused rather than resolved
//! against anything, because nothing carries over between calls to resolve
//! it against. A command that runs and fails (a non-zero exit, `ENOSPC` on a
//! full scratch) is a normal result; `error` is set only for the failure
//! classes the plugin spec names.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use environment::{
    Blob, Cursor, Denial, Edit, Exec, Fault, Outcome, Patch, PatchOp, ProcId, Replace, RootedPath,
    Signal, Window,
};
use harness::tools::render;
use plugin::manifest::ToolDefinition;
use plugin::{Failure, ToolResult};
use serde_json::{Value, json};

use super::places::{Bound, Unavailable};
use super::runtime::Call;

/// How long `exec` waits when the call does not say.
pub const DEFAULT_EXEC_WAIT: Duration = Duration::from_secs(120);
/// The longest any call waits on a command.
pub const MAX_WAIT: Duration = Duration::from_secs(600);
/// How long past its timeout `exec` waits for the last look at its command,
/// before taking the target to be unresponsive.
const EXEC_GRACE: Duration = Duration::from_secs(10);

/// The rules every tool shares, at the start of every tool's description.
/// Static, so the descriptions never change with the project's environments;
/// which environments exist is said in the head.
pub const PREAMBLE: &str = "Every tool takes `execute_in`, naming one environment; it is never defaulted. Every path and every `cwd` is absolute; nothing carries over between calls. When a call fails, the error says whether changing the call can help; if it says not, do not retry it.";

fn described(text: &str) -> String {
    format!("{PREAMBLE}\n\n{text}")
}

pub fn definitions() -> Vec<ToolDefinition> {
    let execute_in = json!({
        "type": "string",
        "description": "The environment to run in, by name, as listed in the head. Required; never defaulted."
    });
    let cwd = json!({
        "type": "string",
        "minLength": 1,
        "description": "Absolute path the command starts in. Applies to this call only."
    });
    let env = json!({
        "type": "object",
        "additionalProperties": { "type": "string" },
        "description": "Extra environment variables for this command only."
    });
    vec![
        ToolDefinition {
            name: "exec".into(),
            description: described(
                "Run a shell command in one environment and wait for it; each call is a new shell. If it is still running after `timeout_ms`, it is not killed: the result gives a process `id` and the output so far, and `process` takes it from there. A non-zero exit is a result, not an error.",
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execute_in": execute_in,
                    "command": { "type": "string", "minLength": 1 },
                    "cwd": cwd,
                    "timeout_ms": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT.as_millis() as u64 },
                    "env": env,
                    "stdin": { "type": "string", "description": "Written to the command's stdin, followed by EOF." }
                },
                "required": ["execute_in", "command", "cwd"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "process".into(),
            description: described(
                "Supervised processes in one environment. `start` launches one in `cwd` (optionally waiting `wait_ms` for output); `list` shows them; `output` reads stdout/stderr from byte offsets (optionally waiting `wait_ms` for new output or exit); `stdin` writes `data` and, with `close`, sends EOF; `signal` sends `signal`. Process ids are stable across calls while the environment stays connected.",
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execute_in": execute_in,
                    "op": { "enum": ["start", "list", "output", "stdin", "signal"] },
                    "id": { "type": "string" },
                    "command": { "type": "string", "minLength": 1 },
                    "cwd": cwd,
                    "env": env,
                    "from_stdout": { "type": "integer", "minimum": 0 },
                    "from_stderr": { "type": "integer", "minimum": 0 },
                    "wait_ms": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT.as_millis() as u64 },
                    "data": { "type": "string" },
                    "close": { "type": "boolean" },
                    "signal": { "enum": ["int", "term", "kill", "hup", "quit", "usr1", "usr2"] }
                },
                "required": ["execute_in", "op"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "read".into(),
            description: described(
                "Read a file in one environment by absolute `path`, whole or as a window of lines (`offset` is the first line, from 0; `limit` is how many).",
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execute_in": execute_in,
                    "path": { "type": "string", "minLength": 1 },
                    "offset": { "type": "integer", "minimum": 0 },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["execute_in", "path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "patch".into(),
            description: described(
                "Change files in one environment; every path is absolute. `ops` run in order and are not atomic: `add` writes `content` to a new or existing `path`; `update` replaces `old` with `new` in `path` (exactly one match unless `all`); `delete` removes `path`; `move` renames `from` to `to`.",
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execute_in": execute_in,
                    "ops": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "enum": ["add", "update", "delete", "move"] },
                                "path": { "type": "string" },
                                "content": { "type": "string" },
                                "old": { "type": "string" },
                                "new": { "type": "string" },
                                "all": { "type": "boolean" },
                                "from": { "type": "string" },
                                "to": { "type": "string" }
                            },
                            "required": ["op"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["execute_in", "ops"],
                "additionalProperties": false
            }),
        },
    ]
}

/// One call, resolved: where it runs, and the name the agent called it by.
/// The name is the config entry's, not the binding's, so a scratch entry
/// named something else is called that in every result.
pub struct Place<'a> {
    pub bound: &'a Bound,
    pub name: &'a str,
}

/// What a call produced besides its result: processes it started, for the
/// exit notices.
#[derive(Default)]
pub struct Effects {
    pub started: Vec<ProcId>,
    /// Those of `started` that `process start` started: a running one keeps
    /// scratch from being idle.
    pub managed: Vec<ProcId>,
}

/// What a call needs of scratch, so its container is started before any of
/// the call runs. Looking at a process needs none; everything else does.
pub fn call_kind(tool: &str, input: &Value) -> Call {
    match tool {
        "process" if input.get("op").and_then(Value::as_str) != Some("start") => Call::Process,
        _ => Call::Container,
    }
}

/// The checks a call's shape can fail without knowing where it runs: a
/// missing or relative `cwd`, a relative path. Run before the call is routed,
/// so a call refused for its shape never opens scratch or starts its container.
pub fn precheck(tool: &str, input: &Value) -> Result<(), ToolResult> {
    let refuse = |message: String| bad_request(message);
    let cwd = || -> Result<(), ToolResult> {
        let Some(cwd) = input.get("cwd").and_then(Value::as_str) else {
            return Err(refuse(CWD_REQUIRED.to_owned()));
        };
        absolute("cwd", cwd).map_err(refuse)
    };
    match tool {
        "exec" => cwd(),
        "process" if input.get("op").and_then(Value::as_str) == Some("start") => {
            if input.get("command").is_none() {
                return Err(refuse("op `start` needs `command`".to_owned()));
            }
            cwd()
        }
        "read" => absolute(
            "path",
            input
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
        .map_err(refuse),
        "patch" => {
            let ops = input.get("ops").and_then(Value::as_array);
            for (index, op) in ops.into_iter().flatten().enumerate() {
                let keys: &[&str] = match op.get("op").and_then(Value::as_str) {
                    Some("add" | "update" | "delete") => &["path"],
                    Some("move") => &["from", "to"],
                    _ => &[],
                };
                for key in keys {
                    if let Some(raw) = op.get(*key).and_then(Value::as_str) {
                        absolute(key, raw)
                            .map_err(|message| refuse(format!("ops[{index}]: {message}")))?;
                    }
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

const CWD_REQUIRED: &str = "`cwd` is required: give the absolute path the command starts in";

/// Runs one tool call where it resolved to.
pub async fn dispatch(
    place: &Place<'_>,
    tool: &str,
    input: &Value,
    effects: &mut Effects,
) -> ToolResult {
    match tool {
        "exec" => exec(place, input, effects).await,
        "process" => process(place, input, effects).await,
        "read" => read(place, input).await,
        "patch" => patch(place, input).await,
        other => bad_request(format!("`{other}` is not an environments tool")),
    }
}

pub fn bad_request(text: impl AsRef<str>) -> ToolResult {
    ToolResult::failed(Failure::BadRequest, text.as_ref())
}

/// A fault from the environment layer, in the plugin's failure classes.
pub fn fault(place: &Place<'_>, fault: &Fault) -> ToolResult {
    match fault {
        Fault::Denied(Denial::NoSuchProcess(id)) => bad_request(format!(
            "`{}` has no process {}; `process` with op `list` shows the ones it has",
            place.name,
            place.bound.process_id(*id)
        )),
        Fault::Denied(Denial::Stopped(id)) => bad_request(format!(
            "process {} in `{}` was stopped because the environment went idle; start it again if you still need it",
            place.bound.process_id(*id),
            place.name
        )),
        Fault::Denied(_) => ToolResult::failed(Failure::BadRequest, render::fault(fault)),
        Fault::Unreachable(reason) if place.bound.is_scratch() => ToolResult::failed(
            Failure::HandlerFailure,
            internal_text(
                &format!("`{}` failed during the call", place.name),
                reason,
                MAY_HAVE_RUN,
            ),
        ),
        Fault::Unreachable(reason) if place.bound.over_ssh => ToolResult::failed(
            Failure::UpstreamFailure,
            unreachable_text(
                &format!("`{}` is unreachable over SSH: {reason}", place.name),
                MAY_HAVE_RUN,
            ),
        ),
        Fault::Unreachable(reason) => ToolResult::failed(
            Failure::UpstreamFailure,
            unreachable_text(
                &format!(
                    "`{}` stopped answering during the call: {reason}",
                    place.name
                ),
                MAY_HAVE_RUN,
            ),
        ),
    }
}

const DID_NOT_RUN: &str = "Nothing ran.";
const MAY_HAVE_RUN: &str = "The call may or may not have taken effect.";

/// A failure inside Faber, worded so the model does not take the failing
/// layer's own vocabulary (`PermissionDenied`, a path, a profile name) as
/// something it can repair. The detail stays, for the user to pass on.
fn internal_text(what: &str, detail: &str, ran: &str) -> String {
    format!(
        "Faber had an internal error: {what}. {ran} Your call did not cause this and you cannot fix it, even if the details below mention a permission or a path. Do not retry, change the command or paths, or try to work around it. Tell the user this environment is failing and include the details.\n\nDetails: {detail}"
    )
}

fn unreachable_text(what: &str, ran: &str) -> String {
    format!(
        "Could not reach the environment: {what}\n{ran} Your call did not cause this, so changing it will not help. Try again later, or tell the user."
    )
}

/// The failure for a call that never found anywhere to run.
pub fn unavailable(why: &Unavailable) -> ToolResult {
    match why {
        Unavailable::Missing(text) => bad_request(text),
        Unavailable::Unreachable(text) => ToolResult::failed(
            Failure::UpstreamFailure,
            unreachable_text(text, DID_NOT_RUN),
        ),
        Unavailable::Settings(text) => ToolResult::failed(
            Failure::UpstreamFailure,
            format!(
                "This environment cannot be used: {text}\n{DID_NOT_RUN} Your call did not cause this. The user has to fix it in their environment settings, so do not retry; tell them what is wrong."
            ),
        ),
        Unavailable::Internal(text) => ToolResult::failed(
            Failure::HandlerFailure,
            internal_text("this environment could not be set up", text, DID_NOT_RUN),
        ),
    }
}

/// A failure of this plugin itself, before the call reached any environment.
pub fn handler_failed(detail: impl AsRef<str>) -> ToolResult {
    ToolResult::failed(
        Failure::HandlerFailure,
        internal_text(
            "the environments tools could not handle this call",
            detail.as_ref(),
            DID_NOT_RUN,
        ),
    )
}

pub async fn exec(place: &Place<'_>, input: &Value, effects: &mut Effects) -> ToolResult {
    let mut request = match request(place, input) {
        Ok(request) => request,
        Err(result) => return result,
    };
    let stdin = input.get("stdin").and_then(Value::as_str);
    request.stdin = stdin.map(|text| Blob::from(text.as_bytes().to_vec()));
    let wait = millis(input, "timeout_ms").unwrap_or(DEFAULT_EXEC_WAIT);

    // `timeout_ms` bounds the whole call, starting included: a target that
    // stops answering while the command starts must not hold the call, and
    // the run waiting on it, forever.
    let target = &place.bound.target;
    let started = Instant::now();
    let deadline = started + wait;
    let left = || deadline.saturating_duration_since(Instant::now());
    let id = match tokio::time::timeout(wait, target.start(request)).await {
        Ok(Ok(id)) => id,
        Ok(Err(error)) => return fault(place, &error),
        Err(_) => {
            return fault(
                place,
                &Fault::Unreachable(format!(
                    "the command did not start within the {:.1}s timeout",
                    wait.as_secs_f64()
                )),
            );
        }
    };
    // Exec's stdin is what the call gave and then EOF, never a pipe left open.
    let _ = tokio::time::timeout(left(), target.close_stdin(id)).await;

    let waited = tokio::time::timeout(
        left() + EXEC_GRACE,
        wait_for(place, id, Cursor::START, left(), true),
    )
    .await
    .unwrap_or_else(|_| {
        Err(Fault::Unreachable(format!(
            "`{}` did not answer for process `{}` within the {:.1}s timeout; it may still be running, and `process` with op `output` reads it",
            place.name,
            place.bound.process_id(id),
            wait.as_secs_f64()
        )))
    });
    match waited {
        Ok(chunk) => {
            let mut out = String::new();
            if chunk.outcome.is_none() {
                effects.started.push(id);
                let _ = writeln!(
                    out,
                    "still running after {:.1}s as process `{}` — not killed. Read more with `process` op `output` (id `{}`, from_stdout={}, from_stderr={}), or stop it with op `signal`.\n",
                    started.elapsed().as_secs_f64(),
                    place.bound.process_id(id),
                    place.bound.process_id(id),
                    chunk.next.stdout,
                    chunk.next.stderr,
                );
            }
            out.push_str(&render::chunk(&chunk, place.bound.blobs.as_ref()));
            ToolResult::ok(out)
        }
        Err(error) => fault(place, &error),
    }
}

pub async fn process(place: &Place<'_>, input: &Value, effects: &mut Effects) -> ToolResult {
    let op = input.get("op").and_then(Value::as_str).unwrap_or_default();
    let allowed: &[&str] = match op {
        "start" => &["command", "cwd", "env", "wait_ms"],
        "list" => &[],
        "output" => &["id", "from_stdout", "from_stderr", "wait_ms"],
        "stdin" => &["id", "data", "close"],
        "signal" => &["id", "signal"],
        _ => &[],
    };
    let stray: Vec<&str> = input
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, _)| key.as_str())
        .filter(|key| !matches!(*key, "execute_in" | "op") && !allowed.contains(key))
        .collect();
    if !stray.is_empty() {
        return bad_request(format!(
            "op `{op}` does not take {}; it takes {}",
            stray
                .iter()
                .map(|key| format!("`{key}`"))
                .collect::<Vec<_>>()
                .join(", "),
            if allowed.is_empty() {
                "nothing else".to_owned()
            } else {
                allowed
                    .iter()
                    .map(|key| format!("`{key}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
    }

    let target = &place.bound.target;
    match op {
        "start" => {
            if input.get("command").is_none() {
                return bad_request("op `start` needs `command`");
            }
            let request = match request(place, input) {
                Ok(request) => request,
                Err(result) => return result,
            };
            let id = match target.start(request).await {
                Ok(id) => id,
                Err(error) => return fault(place, &error),
            };
            effects.started.push(id);
            effects.managed.push(id);
            let mut out = format!(
                "{}: started process `{}`.",
                place.name,
                place.bound.process_id(id)
            );
            if let Some(wait) = millis(input, "wait_ms") {
                match wait_for(place, id, Cursor::START, wait, false).await {
                    Ok(chunk) => {
                        out.push('\n');
                        out.push_str(&render::chunk(&chunk, place.bound.blobs.as_ref()));
                    }
                    Err(error) => return fault(place, &error),
                }
            }
            ToolResult::ok(out)
        }
        "list" => match target.processes().await {
            Ok(processes) if processes.is_empty() => {
                ToolResult::ok(format!("{}: no processes", place.name))
            }
            Ok(processes) => {
                let mut out = format!("{}: {} processes\n", place.name, processes.len());
                for process in processes {
                    let state = match process.outcome {
                        None => "running".to_owned(),
                        Some(outcome) => ended(&outcome),
                    };
                    let _ = writeln!(
                        out,
                        "- `{}` {state} — `{}` in {} (stdout {} bytes, stderr {} bytes)",
                        place.bound.process_id(process.id),
                        process.command,
                        process.cwd,
                        process.stdout_len,
                        process.stderr_len,
                    );
                }
                ToolResult::ok(out)
            }
            Err(error) => fault(place, &error),
        },
        "output" => {
            let id = match process_id(place, input) {
                Ok(id) => id,
                Err(result) => return result,
            };
            let from = Cursor {
                stdout: input
                    .get("from_stdout")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                stderr: input
                    .get("from_stderr")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            };
            let wait = millis(input, "wait_ms").unwrap_or(Duration::ZERO);
            match wait_for(place, id, from, wait, false).await {
                Ok(chunk) => ToolResult::ok(render::chunk(&chunk, place.bound.blobs.as_ref())),
                Err(error) => fault(place, &error),
            }
        }
        "stdin" => {
            let id = match process_id(place, input) {
                Ok(id) => id,
                Err(result) => return result,
            };
            let data = input.get("data").and_then(Value::as_str);
            let close = input.get("close").and_then(Value::as_bool).unwrap_or(false);
            if data.is_none() && !close {
                return bad_request("op `stdin` needs `data`, `close`, or both");
            }
            if let Some(data) = data
                && let Err(error) = target
                    .stdin(id, &Blob::from(data.as_bytes().to_vec()))
                    .await
            {
                return fault(place, &error);
            }
            if close && let Err(error) = target.close_stdin(id).await {
                return fault(place, &error);
            }
            ToolResult::ok(format!(
                "{}: wrote {} bytes to process `{}`{}",
                place.name,
                data.map_or(0, str::len),
                place.bound.process_id(id),
                if close { " and closed its stdin" } else { "" }
            ))
        }
        "signal" => {
            let id = match process_id(place, input) {
                Ok(id) => id,
                Err(result) => return result,
            };
            let signal = match input.get("signal").and_then(Value::as_str) {
                Some("int") => Signal::Int,
                Some("term") => Signal::Term,
                Some("kill") => Signal::Kill,
                Some("hup") => Signal::Hup,
                Some("quit") => Signal::Quit,
                Some("usr1") => Signal::Usr1,
                Some("usr2") => Signal::Usr2,
                _ => return bad_request("op `signal` needs `signal`"),
            };
            match target.signal(id, signal).await {
                Ok(()) => ToolResult::ok(format!(
                    "{}: sent {signal} to process `{}`",
                    place.name,
                    place.bound.process_id(id)
                )),
                Err(error) => fault(place, &error),
            }
        }
        other => bad_request(format!("`{other}` is not an op")),
    }
}

pub async fn read(place: &Place<'_>, input: &Value) -> ToolResult {
    let path = match path(place, input, "path") {
        Ok(path) => path,
        Err(result) => return result,
    };
    let offset = input.get("offset").and_then(Value::as_u64);
    let limit = input.get("limit").and_then(Value::as_u64);
    let window = match (offset, limit) {
        (None, None) => None,
        (offset, limit) => Some(Window::new(offset.unwrap_or(0), limit.unwrap_or(2000))),
    };
    match place.bound.target.read(&path, window).await {
        Ok(span) => ToolResult::ok(render::read(
            place.name,
            path.as_str(),
            &span,
            place.bound.blobs.as_ref(),
        )),
        Err(error) => fault(place, &error),
    }
}

pub async fn patch(place: &Place<'_>, input: &Value) -> ToolResult {
    let ops = input
        .get("ops")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut parsed = Vec::with_capacity(ops.len());
    for (index, op) in ops.iter().enumerate() {
        match patch_op(place, op) {
            Ok(op) => parsed.push(op),
            Err(message) => return bad_request(format!("ops[{index}]: {message}")),
        }
    }
    match place
        .bound
        .target
        .edit(&Edit::Patch(Patch::new(parsed)))
        .await
    {
        Ok(stats) => ToolResult::ok(render::stats(place.name, &stats)),
        Err(error) => fault(place, &error),
    }
}

fn patch_op(place: &Place<'_>, op: &Value) -> Result<PatchOp, String> {
    let field = |key: &str| -> Result<String, String> {
        op.get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("`{key}` is required"))
    };
    let at = |key: &str| -> Result<RootedPath, String> {
        let raw = field(key)?;
        absolute(key, &raw)?;
        place
            .bound
            .target
            .path(&raw)
            .map_err(|fault| render::fault(&fault))
    };
    match op.get("op").and_then(Value::as_str).unwrap_or_default() {
        "add" => Ok(PatchOp::Add {
            path: at("path")?,
            body: field("content")?.into_bytes(),
        }),
        "update" => Ok(PatchOp::Update(Replace {
            path: at("path")?,
            old: field("old")?,
            new: field("new")?,
            all: op.get("all").and_then(Value::as_bool).unwrap_or(false),
        })),
        "delete" => Ok(PatchOp::Delete { path: at("path")? }),
        "move" => Ok(PatchOp::Move {
            from: at("from")?,
            to: at("to")?,
        }),
        other => Err(format!(
            "`{other}` is not an operation; use add, update, delete, or move"
        )),
    }
}

/// An [`Exec`] from a call's `command`, `cwd` and `env`.
fn request(place: &Place<'_>, input: &Value) -> Result<Exec, ToolResult> {
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut request = Exec::new(command);
    let Some(cwd) = input.get("cwd").and_then(Value::as_str) else {
        return Err(bad_request(CWD_REQUIRED));
    };
    absolute("cwd", cwd).map_err(|message| bad_request(message))?;
    request.cwd = Some(
        place
            .bound
            .target
            .path(cwd)
            .map_err(|error| fault(place, &error))?,
    );
    if let Some(env) = input.get("env").and_then(Value::as_object) {
        for (key, value) in env {
            request
                .env
                .push((key.clone(), value.as_str().unwrap_or_default().to_owned()));
        }
    }
    // The wait is the call's; the process itself has no deadline.
    request.timeout = MAX_WAIT;
    Ok(request)
}

fn path(place: &Place<'_>, input: &Value, key: &str) -> Result<RootedPath, ToolResult> {
    let raw = input.get(key).and_then(Value::as_str).unwrap_or_default();
    absolute(key, raw).map_err(|message| bad_request(message))?;
    place
        .bound
        .target
        .path(raw)
        .map_err(|error| fault(place, &error))
}

/// Refuses a relative path or `cwd` by name. Paths are the environment's own
/// real paths, and there is no working directory between calls to take a
/// relative one from, so guessing one would run somewhere the agent did not
/// say.
fn absolute(key: &str, raw: &str) -> Result<(), String> {
    if raw.starts_with('/') {
        Ok(())
    } else {
        Err(format!(
            "`{key}` must be an absolute path; `{raw}` is relative"
        ))
    }
}

fn process_id(place: &Place<'_>, input: &Value) -> Result<ProcId, ToolResult> {
    let Some(raw) = input.get("id").and_then(Value::as_str) else {
        return Err(bad_request("this op needs `id`"));
    };
    place.bound.parse_process_id(raw).ok_or_else(|| {
        bad_request(format!(
            "`{}` has no process `{raw}`; `process` with op `list` shows the ones it has",
            place.name
        ))
    })
}

fn millis(input: &Value, key: &str) -> Option<Duration> {
    input
        .get(key)
        .and_then(Value::as_u64)
        .map(|ms| Duration::from_millis(ms).min(MAX_WAIT))
}

/// Reads output from `from`, waiting up to `wait` for the process to exit or —
/// unless `until_exit` — for any new output.
async fn wait_for(
    place: &Place<'_>,
    id: ProcId,
    from: Cursor,
    wait: Duration,
    until_exit: bool,
) -> Result<environment::Chunk, Fault> {
    let deadline = Instant::now() + wait;
    let mut pause = Duration::from_millis(25);
    loop {
        let chunk = place.bound.target.output(id, from).await?;
        let has_new = chunk.next.stdout > from.stdout || chunk.next.stderr > from.stderr;
        if chunk.outcome.is_some() || (!until_exit && has_new) || Instant::now() >= deadline {
            // Once exited, one more read picks up anything still in the pipes.
            if chunk.outcome.is_some() {
                tokio::time::sleep(Duration::from_millis(20)).await;
                return place.bound.target.output(id, from).await;
            }
            return Ok(chunk);
        }
        tokio::time::sleep(pause.min(deadline.saturating_duration_since(Instant::now()))).await;
        pause = (pause * 2).min(Duration::from_millis(500));
    }
}

fn ended(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Completed { code } => format!("exited {code}"),
        Outcome::TimedOut => "timed out".to_owned(),
        Outcome::Signaled { signal } => format!("killed by {signal}"),
    }
}

pub fn exit_line(name: &str, process: &str, outcome: &Outcome) -> String {
    format!("process `{process}` on `{name}` {}", ended(outcome))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use environment::{Blobs, LocalTarget, Root, Target};

    use super::*;
    use crate::plugins::environments::places::RecentBlobs;

    async fn bound(dir: &std::path::Path) -> Bound {
        let blobs = Arc::new(RecentBlobs::default());
        let machine = LocalTarget::bind(
            "box",
            Root::new(dir.to_string_lossy()).unwrap(),
            Arc::clone(&blobs) as Arc<dyn Blobs>,
        )
        .await
        .unwrap();
        Bound::new("box", Arc::new(machine) as Arc<dyn Target>, blobs)
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "faber-tools-{name}-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn text(result: &ToolResult) -> String {
        plugin::types::joined(&result.content)
    }

    #[test]
    fn every_description_starts_with_the_preamble() {
        for tool in definitions() {
            assert!(tool.description.starts_with(PREAMBLE), "{}", tool.name);
        }
        let exec = &definitions()[0];
        assert_eq!(
            exec.input_schema["required"],
            json!(["execute_in", "command", "cwd"])
        );
    }

    #[test]
    fn every_relative_path_is_refused_before_the_call_is_routed() {
        let refused = |tool: &str, input: Value| {
            let result = precheck(tool, &input).expect_err(&input.to_string());
            assert!(result.is_error());
            text(&result)
        };
        assert_eq!(
            refused("exec", json!({ "command": "true" })),
            "`cwd` is required: give the absolute path the command starts in"
        );
        assert_eq!(
            refused(
                "process",
                json!({ "op": "start", "command": "sleep 1", "cwd": "work" })
            ),
            "`cwd` must be an absolute path; `work` is relative"
        );
        assert_eq!(
            refused("process", json!({ "op": "start", "cwd": "/w" })),
            "op `start` needs `command`"
        );
        for op in ["add", "update", "delete"] {
            assert_eq!(
                refused(
                    "patch",
                    json!({ "ops": [{ "op": "add", "path": "/ok" }, { "op": op, "path": "a.txt" }] })
                ),
                "ops[1]: `path` must be an absolute path; `a.txt` is relative"
            );
        }
        assert_eq!(
            refused(
                "patch",
                json!({ "ops": [{ "op": "move", "from": "a", "to": "/b" }] })
            ),
            "ops[0]: `from` must be an absolute path; `a` is relative"
        );
        assert_eq!(
            refused(
                "patch",
                json!({ "ops": [{ "op": "move", "from": "/a", "to": "b" }] })
            ),
            "ops[0]: `to` must be an absolute path; `b` is relative"
        );
        assert_eq!(
            refused("read", json!({ "path": "notes.md" })),
            "`path` must be an absolute path; `notes.md` is relative"
        );
        // Shape only: the place a call names is not looked at here, so
        // scratch is refused the same way, before it could be opened.
        assert!(
            precheck(
                "exec",
                &json!({ "execute_in": "scratch", "command": "x", "cwd": "rel" })
            )
            .is_err()
        );
        assert!(precheck("exec", &json!({ "command": "x", "cwd": "/root" })).is_ok());
        assert!(precheck("process", &json!({ "op": "list" })).is_ok());
    }

    /// A target that never answers a start, as scratch did over a frozen
    /// sandbox. Nothing else is called.
    struct Unanswering(Bound);

    #[async_trait::async_trait]
    impl Target for Unanswering {
        fn manifest(&self) -> &environment::Manifest {
            self.0.target.manifest()
        }
        async fn exec(&self, _: Exec) -> Result<environment::Exit, Fault> {
            unreachable!()
        }
        async fn start(&self, _: Exec) -> Result<ProcId, Fault> {
            std::future::pending().await
        }
        async fn output(&self, _: ProcId, _: Cursor) -> Result<environment::Chunk, Fault> {
            unreachable!()
        }
        async fn stdin(&self, _: ProcId, _: &Blob) -> Result<(), Fault> {
            unreachable!()
        }
        async fn close_stdin(&self, _: ProcId) -> Result<(), Fault> {
            unreachable!()
        }
        async fn processes(&self) -> Result<Vec<environment::Process>, Fault> {
            unreachable!()
        }
        async fn signal(&self, _: ProcId, _: environment::Signal) -> Result<(), Fault> {
            unreachable!()
        }
        async fn read(
            &self,
            _: &RootedPath,
            _: Option<environment::Window>,
        ) -> Result<environment::Span, Fault> {
            unreachable!()
        }
        async fn write(&self, _: &RootedPath, _: &Blob) -> Result<environment::Stat, Fault> {
            unreachable!()
        }
        async fn edit(&self, _: &Edit) -> Result<Vec<environment::Stat>, Fault> {
            unreachable!()
        }
        async fn list(
            &self,
            _: &RootedPath,
            _: Option<&str>,
        ) -> Result<environment::Listing, Fault> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn exec_timeout_covers_a_start_that_never_answers() {
        let dir = temp("unanswered");
        let local = bound(&dir).await;
        let blobs = Arc::clone(&local.blobs);
        let bound = Bound::new(
            "box",
            Arc::new(Unanswering(local)) as Arc<dyn Target>,
            blobs,
        );
        let place = Place {
            bound: &bound,
            name: "local",
        };
        let cwd = dir.to_string_lossy().into_owned();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            exec(
                &place,
                &json!({ "command": "true", "cwd": cwd, "timeout_ms": 200 }),
                &mut Effects::default(),
            ),
        )
        .await
        .expect("exec outlived its timeout_ms");
        assert!(result.is_error());
        assert!(
            text(&result).contains("did not start within"),
            "{}",
            text(&result)
        );
    }

    #[tokio::test]
    async fn relative_paths_and_a_missing_cwd_are_refused_not_resolved() {
        let dir = temp("relative");
        std::fs::write(dir.join("a.txt"), "hi").unwrap();
        let bound = bound(&dir).await;
        let place = Place {
            bound: &bound,
            name: "local",
        };
        let mut effects = Effects::default();

        let result = exec(&place, &json!({ "command": "true" }), &mut effects).await;
        assert!(result.is_error());
        assert!(
            text(&result).contains("`cwd` is required"),
            "{}",
            text(&result)
        );

        let result = exec(
            &place,
            &json!({ "command": "true", "cwd": "src" }),
            &mut effects,
        )
        .await;
        assert!(
            text(&result).contains("`cwd` must be an absolute path"),
            "{}",
            text(&result)
        );

        let result = process(
            &place,
            &json!({ "op": "start", "command": "true" }),
            &mut effects,
        )
        .await;
        assert!(
            text(&result).contains("`cwd` is required"),
            "{}",
            text(&result)
        );

        let result = read(&place, &json!({ "path": "a.txt" })).await;
        assert!(text(&result).starts_with("`path` must be an absolute path"));

        let result = patch(
            &place,
            &json!({ "ops": [{ "op": "move", "from": format!("{}/a.txt", dir.display()), "to": "b.txt" }] }),
        )
        .await;
        assert!(
            text(&result).contains("`to` must be an absolute path"),
            "{}",
            text(&result)
        );

        let result = read(
            &place,
            &json!({ "path": format!("{}/a.txt", dir.display()) }),
        )
        .await;
        assert!(!result.is_error(), "{}", text(&result));
        assert!(text(&result).contains("hi"));

        let result = exec(
            &place,
            &json!({ "command": "pwd", "cwd": dir.to_string_lossy() }),
            &mut effects,
        )
        .await;
        assert!(!result.is_error(), "{}", text(&result));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failure_inside_faber_tells_the_model_to_stop_and_keeps_the_detail() {
        let detail = "PermissionDenied: denied by profiles.scratch.volumes: no volume slot";
        let result = unavailable(&Unavailable::Internal(detail.to_owned()));
        assert_eq!(result.error, Some(Failure::HandlerFailure));
        let text = text(&result);
        assert!(text.starts_with("Faber had an internal error: "), "{text}");
        assert!(text.contains("Your call did not cause this"), "{text}");
        assert!(text.contains("Do not retry"), "{text}");
        assert!(text.ends_with(&format!("Details: {detail}")), "{text}");
    }

    #[test]
    fn each_kind_of_unavailable_says_whose_problem_it_is() {
        let of = |why: Unavailable| text(&unavailable(&why));
        assert_eq!(of(Unavailable::Missing("no `x`".into())), "no `x`");
        let offline = of(Unavailable::Unreachable("`box` is offline".into()));
        assert!(offline.starts_with("Could not reach the environment: `box` is offline"));
        assert!(offline.contains("Try again later"), "{offline}");
        let settings = of(Unavailable::Settings("`box` is disabled".into()));
        assert!(settings.starts_with("This environment cannot be used: `box` is disabled"));
        assert!(settings.contains("environment settings"), "{settings}");
    }

    #[tokio::test]
    async fn a_scratch_transport_fault_is_internal_and_a_machines_is_unreachable() {
        let dir = temp("faults");
        let mut bound = bound(&dir).await;
        let gone = Fault::Unreachable("connection reset".into());

        let place = Place {
            bound: &bound,
            name: "scratch",
        };
        assert!(
            text(&fault(&place, &gone))
                .starts_with("Faber had an internal error: `scratch` failed")
        );

        bound.over_ssh = true;
        bound.environment_id = Some(uuid::Uuid::nil());
        let place = Place {
            bound: &bound,
            name: "box",
        };
        assert!(
            text(&fault(&place, &gone)).contains("`box` is unreachable over SSH: connection reset")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_looking_at_a_process_leaves_the_container_alone() {
        let kind = |tool, input: Value| call_kind(tool, &input);
        assert_eq!(kind("exec", json!({})), Call::Container);
        assert_eq!(kind("read", json!({})), Call::Container);
        assert_eq!(kind("patch", json!({})), Call::Container);
        assert_eq!(kind("process", json!({"op": "start"})), Call::Container);
        assert_eq!(kind("process", json!({"op": "list"})), Call::Process);
        assert_eq!(kind("process", json!({"op": "output"})), Call::Process);
    }
}
