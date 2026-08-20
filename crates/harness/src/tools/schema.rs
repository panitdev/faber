//! The tool definitions, as bytes in the prefix.
//!
//! Three properties this file exists to hold still.
//!
//! **The schema is a constant.** It is not built from the bindings, not
//! filtered by what any environment can do, and not rebuilt when an environment is bound
//! mid-session. Tool definitions sit near the top of the request and every
//! byte after them is cached against them, so a schema that varied with the
//! binding set would invalidate the whole prefix each time a user added an
//! environment — and would make binding mid-run impossible rather than merely
//! expensive. Capability lives in the manifest, which is data at the end of
//! the context, and a call against an environment that cannot answer it is denied
//! with the capability named.
//!
//! **`execute_in` is required on every call and never defaulted.** It is a plain
//! string, not an enum of bound labels — an enum would put the bindings back
//! into the schema through the side door. A defaulted environment means a forgotten
//! parameter silently routes to a machine the model was not thinking about,
//! and the failures that produces are the destructive ones: an `rm -rf`, a
//! migration, a service restart on the wrong host. An omitted required
//! parameter is a validation error the model repairs in one turn.
//!
//! **A description says what the model must know before it calls; the result
//! says what happened.** Every sentence here is prefix rent paid on every
//! request of every session, and a sentence describing the *shape of the
//! answer* is rent paid for something [`render`](super::render) already says on
//! the one turn it matters. So: no restating that a nonzero exit is a result,
//! that a truncated listing says so, that a refusal is deterministic. What
//! stays is what cannot be recovered from a result the model has not received
//! yet — that `cwd` does not persist, that a patch is not atomic, that `old`
//! must be unique.

use serde_json::{Value, json};

/// The label of the environment a call runs against. Repeated on every tool
/// because every tool needs it and none of them may infer it.
fn execute_in_property() -> Value {
    json!({
        "type": "string",
        "description": "The bound environment to execute in. Call `bound_environments` to list them.",
    })
}

/// Where a file lives, or where a command runs.
///
/// Factored out rather than repeated: nine verbatim copies of this sentence
/// were a quarter of all the description bytes in this file. The example is
/// the instruction — an absolute target path is explicit, so there is no
/// separate virtual namespace to explain.
fn path_property(extra: &str) -> Value {
    let mut description = "Absolute path on the target, under the root reported by \
                           `bound_environments`, e.g. `/work/src/main.rs`."
        .to_owned();
    if !extra.is_empty() {
        description.push(' ');
        description.push_str(extra);
    }
    json!({ "type": "string", "description": description })
}

fn tool(name: &str, description: &str, mut properties: Value, required: &[&str]) -> llm::ToolDef {
    if let Value::Object(fields) = &mut properties {
        fields.insert("execute_in".to_owned(), execute_in_property());
    }
    let mut required: Vec<&str> = required.to_vec();
    required.insert(0, "execute_in");

    llm::ToolDef {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        }),
    }
}

/// The whole surface, in a fixed order.
///
/// Order is load-bearing: a reordered tool array hashes differently and reads
/// as a changed prefix, so this list is appended to and never rearranged. A
/// tool may still be *removed* at a release boundary — that invalidates the
/// prefix once, the way any prompt change does, and leaves the order of what
/// remains intact.
///
/// It is deliberately short. A verb that a shell command does as well belongs
/// in the shell: `ls` needs no `list` tool, and a whole-file write is a patch
/// with one `add` op. What is here is what the shell cannot do (a handle on a
/// running process), what it does worse (a read that tells missing, empty, and
/// past-the-end apart), and the one mutation verb.
pub fn definitions() -> Vec<llm::ToolDef> {
    vec![
        bound_environments(),
        exec(),
        start(),
        output(),
        stdin(),
        signal(),
        read(),
        patch(),
    ]
}

/// The only tool with no `execute_in`, because it is the one that answers what the
/// targets are.
fn bound_environments() -> llm::ToolDef {
    llm::ToolDef {
        name: "bound_environments".to_owned(),
        description: "List the bound environments: os, arch, shell, which verbs each \
                      answers, which tools were found on it, whether it can reach the network, \
                      and whether its filesystem boundary is container-enforced."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        }),
    }
}

fn exec() -> llm::ToolDef {
    tool(
        "exec",
        "Run a shell command in an environment and wait for it to finish.\n\
         - Always set `cwd`; it applies to this call only and never persists. Do not use \
         `cd` unless necessary.\n\
         - Large output is also written to a file inside the environment, and the result \
         names the path.",
        json!({
            "command": {
                "type": "string",
                "description": "The command line, run through the environment's shell.",
            },
            "cwd": path_property("Defaults to the root."),
            "timeout_ms": {
                "type": "integer",
                "description": "Milliseconds to wait before killing the command and returning \
                                what it produced. Defaults to 120000.",
                "minimum": 1,
            },
            "env": {
                "type": "object",
                "description": "Extra environment variables for this call.",
                "additionalProperties": { "type": "string" },
            },
            "stdin": {
                "type": "string",
                "description": "Fed to the command's stdin, which is then closed.",
            },
        }),
        &["command"],
    )
}

fn start() -> llm::ToolDef {
    tool(
        "start",
        "Start a command without waiting for it and return a process handle for `output`, \
         `stdin`, and `signal`. Handles live only as long as the connection to the \
         environment.",
        json!({
            "command": { "type": "string" },
            "cwd": path_property("Defaults to the root."),
            "env": {
                "type": "object",
                "additionalProperties": { "type": "string" },
            },
            "stdin": {
                "type": "string",
                "description": "Written to stdin before the handle is returned. Unlike \
                                `exec`, stdin stays open.",
            },
        }),
        &["command"],
    )
}

fn output() -> llm::ToolDef {
    tool(
        "output",
        "Read what a started process has produced since a byte offset, and get back the \
         offset to resume from.",
        json!({
            "process": {
                "type": "integer",
                "description": "The handle `start` returned.",
                "minimum": 0,
            },
            "from_stdout": {
                "type": "integer",
                "description": "Byte offset to resume stdout from. Defaults to 0.",
                "minimum": 0,
            },
            "from_stderr": {
                "type": "integer",
                "description": "Byte offset to resume stderr from. Defaults to 0.",
                "minimum": 0,
            },
        }),
        &["process"],
    )
}

fn stdin() -> llm::ToolDef {
    tool(
        "stdin",
        "Write to a started process's stdin.",
        json!({
            "process": { "type": "integer", "minimum": 0 },
            "data": {
                "type": "string",
                "description": "Written as-is. Include the trailing newline yourself if the \
                                process is waiting for one.",
            },
        }),
        &["process", "data"],
    )
}

fn signal() -> llm::ToolDef {
    tool(
        "signal",
        "Send a signal to a started process.",
        json!({
            "process": { "type": "integer", "minimum": 0 },
            "signal": {
                "type": "string",
                "enum": ["int", "term", "kill", "hup", "quit", "usr1", "usr2"],
            },
        }),
        &["process", "signal"],
    )
}

fn read() -> llm::ToolDef {
    tool(
        "read",
        "Read a file, or a window of lines from one.",
        json!({
            "path": path_property(""),
            "offset": {
                "type": "integer",
                "description": "0-based first line. Requires `limit`.",
                "minimum": 0,
            },
            "limit": {
                "type": "integer",
                "description": "How many lines to read from `offset`.",
                "minimum": 1,
            },
        }),
        &["path"],
    )
}

/// The only way to change a file.
///
/// One mutation verb rather than three. `write` was `add` under another name,
/// and `edit` was a single-op patch — three names for one operation cost more
/// in prefix and in "which one do I use" than the ceremony of an `ops` array
/// costs at the call site.
fn patch() -> llm::ToolDef {
    tool(
        "patch",
        "Change files: create, replace text in, delete, and rename them. Operations run in \
         order and are NOT atomic: if one fails, the ones before it stay applied.\n\
         - `add` writes a whole file, creating or replacing it.\n\
         - `update` replaces `old` with `new`. `old` must match exactly, whitespace \
         included, and must appear exactly once unless `all` is set.",
        json!({
            "ops": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "op": {
                            "type": "string",
                            "enum": ["add", "update", "delete", "move"],
                        },
                        "path": path_property("For add, update, and delete."),
                        "content": { "type": "string", "description": "For add." },
                        "old": { "type": "string", "description": "For update." },
                        "new": { "type": "string", "description": "For update." },
                        "all": { "type": "boolean", "description": "For update." },
                        "from": path_property("The source. For move."),
                        "to": path_property("The destination. For move."),
                    },
                    "required": ["op"],
                    "additionalProperties": false,
                },
            },
        }),
        &["ops"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_but_targets_requires_an_environment_it_cannot_default() {
        for tool in definitions() {
            let required = tool.input_schema["required"].as_array().cloned();
            if tool.name == "bound_environments" {
                assert!(required.is_none(), "`bound_environments` takes no parameters");
                continue;
            }
            let required = required.expect("every other tool has required parameters");
            assert!(
                required.iter().any(|name| name == "execute_in"),
                "`{}` must require execute_in",
                tool.name
            );
            let property = &tool.input_schema["properties"]["execute_in"];
            // A plain string, never an enum: an enum of bound labels would put
            // the binding set back into the prefix, which is exactly what
            // makes mid-session binding cheap to avoid.
            assert_eq!(property["type"], "string");
            assert!(property.get("enum").is_none(), "`{}`", tool.name);
        }
    }

    #[test]
    fn the_schema_does_not_depend_on_what_is_bound() {
        // Nothing to pass in — the definitions are a constant. This test is
        // the statement of that, and it fails to compile the day someone
        // gives `definitions` a parameter.
        assert_eq!(definitions().len(), definitions().len());
        assert!(definitions().iter().any(|tool| tool.name == "exec"));
    }

    /// The surface is what the shell cannot do or does worse. A verb that is
    /// `ls`, or a whole-file write that is one `add` op, does not come back.
    #[test]
    fn nothing_here_duplicates_a_shell_command_or_another_tool() {
        let names: Vec<String> = definitions().into_iter().map(|tool| tool.name).collect();
        for gone in ["list", "write", "edit", "glob", "grep", "find"] {
            assert!(!names.iter().any(|name| name == gone), "`{gone}` is back");
        }
    }

    /// Descriptions are prefix rent. This is a ceiling on the bill, not a
    /// style rule: it catches the drift back toward explaining the *result*
    /// in the *schema*, which is where the last kilobyte went.
    #[test]
    fn no_description_grows_back_into_an_essay() {
        for tool in definitions() {
            assert!(
                tool.description.len() < 400,
                "`{}`: {} chars — say it in the result instead",
                tool.name,
                tool.description.len()
            );
        }
    }
}
