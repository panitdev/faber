//! `exec`, `process`, `read` and `patch`: the declared tools, their schemas,
//! and what each call does once it knows where it runs.
//!
//! `execute_in` is required on every tool and never defaulted. A command that
//! runs and fails (a non-zero exit, `ENOSPC` on a full scratch) is a normal
//! result; `error` is set only for the failure classes the plugin spec names.

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

use super::places::Bound;

/// How long `exec` waits when the call does not say.
pub const DEFAULT_EXEC_WAIT: Duration = Duration::from_secs(120);
/// The longest any call waits on a command.
pub const MAX_WAIT: Duration = Duration::from_secs(600);

pub fn definitions() -> Vec<ToolDefinition> {
    let execute_in = json!({
        "type": "string",
        "description": "The environment to run in, by name: `scratch` or one of the project's machines. Required; never defaulted."
    });
    let env = json!({
        "type": "object",
        "additionalProperties": { "type": "string" },
        "description": "Extra environment variables for this command only."
    });
    vec![
        ToolDefinition {
            name: "exec".into(),
            description: "Run a shell command in one environment and wait for it. If it is still running after `timeout_ms`, it is not killed: the result gives a process `id` and the output so far, and `process` takes it from there. A non-zero exit is a result, not an error.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execute_in": execute_in,
                    "command": { "type": "string", "minLength": 1 },
                    "cwd": { "type": "string", "description": "Where the command starts; defaults to the environment's workdir. Applies to this call only." },
                    "timeout_ms": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT.as_millis() as u64 },
                    "env": env,
                    "stdin": { "type": "string", "description": "Written to the command's stdin, followed by EOF." }
                },
                "required": ["execute_in", "command"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "process".into(),
            description: "Supervised processes in one environment. `start` launches one (optionally waiting `wait_ms` for output); `list` shows them; `output` reads stdout/stderr from byte offsets (optionally waiting `wait_ms` for new output or exit); `stdin` writes `data` and, with `close`, sends EOF; `signal` sends `signal`. Process ids are stable across calls and conversations while the environment stays connected.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "execute_in": execute_in,
                    "op": { "enum": ["start", "list", "output", "stdin", "signal"] },
                    "id": { "type": "string" },
                    "command": { "type": "string", "minLength": 1 },
                    "cwd": { "type": "string" },
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
            description: "Read a file in one environment, whole or as a window of lines (`offset` is the first line, from 0; `limit` is how many).".into(),
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
            description: "Change files in one environment. `ops` run in order and are not atomic: `add` writes `content` to a new or existing `path`; `update` replaces `old` with `new` in `path` (exactly one match unless `all`); `delete` removes `path`; `move` renames `from` to `to`.".into(),
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

/// One call, resolved: where it runs, and that place's default cwd.
pub struct Place<'a> {
    pub bound: &'a Bound,
    pub workdir: Option<&'a str>,
}

/// What a call produced besides its result: processes it started, for the
/// exit notices.
#[derive(Default)]
pub struct Effects {
    pub started: Vec<ProcId>,
}

pub fn bad_request(code: &str, text: impl AsRef<str>) -> ToolResult {
    ToolResult::failed(Failure::BadRequest, format!("{code}: {}", text.as_ref()))
}

/// A fault from the environment layer, in the plugin's failure classes.
pub fn fault(place: &Place<'_>, fault: &Fault) -> ToolResult {
    match fault {
        Fault::Denied(Denial::NoSuchProcess(id)) => bad_request(
            "process_unknown",
            format!(
                "`{}` has no process {}; `process` with op `list` shows the ones it has",
                place.bound.name,
                place.bound.process_id(*id)
            ),
        ),
        Fault::Denied(_) => ToolResult::failed(Failure::BadRequest, render::fault(fault)),
        Fault::Unreachable(reason) => ToolResult::failed(
            Failure::UpstreamFailure,
            format!(
                "env_unreachable: `{}` stopped answering during the call: {reason}",
                place.bound.name
            ),
        ),
    }
}

pub async fn exec(place: &Place<'_>, input: &Value, effects: &mut Effects) -> ToolResult {
    let mut request = match request(place, input) {
        Ok(request) => request,
        Err(result) => return result,
    };
    let stdin = input.get("stdin").and_then(Value::as_str);
    request.stdin = stdin.map(|text| Blob::from(text.as_bytes().to_vec()));
    let wait = millis(input, "timeout_ms").unwrap_or(DEFAULT_EXEC_WAIT);

    let target = &place.bound.target;
    let id = match target.start(request).await {
        Ok(id) => id,
        Err(error) => return fault(place, &error),
    };
    // Exec's stdin is what the call gave and then EOF, never a pipe left open.
    let _ = target.close_stdin(id).await;

    let started = Instant::now();
    match wait_for(place, id, Cursor::START, wait, true).await {
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
        return bad_request(
            "bad_request",
            format!(
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
            ),
        );
    }

    let target = &place.bound.target;
    match op {
        "start" => {
            if input.get("command").is_none() {
                return bad_request("bad_request", "op `start` needs `command`");
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
            let mut out = format!(
                "{}: started process `{}`.",
                place.bound.name,
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
                ToolResult::ok(format!("{}: no processes", place.bound.name))
            }
            Ok(processes) => {
                let mut out = format!("{}: {} processes\n", place.bound.name, processes.len());
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
                return bad_request("bad_request", "op `stdin` needs `data`, `close`, or both");
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
                place.bound.name,
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
                _ => return bad_request("bad_request", "op `signal` needs `signal`"),
            };
            match target.signal(id, signal).await {
                Ok(()) => ToolResult::ok(format!(
                    "{}: sent {signal} to process `{}`",
                    place.bound.name,
                    place.bound.process_id(id)
                )),
                Err(error) => fault(place, &error),
            }
        }
        other => bad_request("bad_request", format!("`{other}` is not an op")),
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
            &place.bound.name,
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
            Err(message) => return bad_request("bad_request", format!("ops[{index}]: {message}")),
        }
    }
    match place
        .bound
        .target
        .edit(&Edit::Patch(Patch::new(parsed)))
        .await
    {
        Ok(stats) => ToolResult::ok(render::stats(&place.bound.name, &stats)),
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
        resolve(place, &field(key)?).map_err(|fault| render::fault(&fault))
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
    let cwd = match input.get("cwd").and_then(Value::as_str) {
        Some(cwd) => Some(cwd.to_owned()),
        None => place.workdir.map(str::to_owned),
    };
    if let Some(cwd) = cwd {
        request.cwd = Some(resolve(place, &cwd).map_err(|error| fault(place, &error))?);
    }
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
    resolve(place, raw).map_err(|error| fault(place, &error))
}

/// Paths are the environment's real paths; a relative one is taken from its
/// workdir.
fn resolve(place: &Place<'_>, raw: &str) -> Result<RootedPath, Fault> {
    if raw.starts_with('/') {
        return place.bound.target.path(raw);
    }
    let base = place.workdir.unwrap_or("/");
    place
        .bound
        .target
        .path(&format!("{}/{raw}", base.trim_end_matches('/')))
}

fn process_id(place: &Place<'_>, input: &Value) -> Result<ProcId, ToolResult> {
    let Some(raw) = input.get("id").and_then(Value::as_str) else {
        return Err(bad_request("bad_request", "this op needs `id`"));
    };
    place.bound.parse_process_id(raw).ok_or_else(|| {
        bad_request(
            "process_unknown",
            format!(
                "`{}` has no process `{raw}`; `process` with op `list` shows the ones it has",
                place.bound.name
            ),
        )
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
