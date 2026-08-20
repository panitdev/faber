//! The standard tool surface, granted to the shipped conversational harness.
//!
//! The claim under test is the one the surface exists to make: a harness gets
//! a working environment by being granted it, and implements no tools of its
//! own. So these run the real `CONVERSATIONAL` source against a real
//! `LocalTarget` over a temporary directory, with only the model scripted.

mod support;

use std::sync::Arc;

use environment::{Blobs, LocalTarget, MemoryBlobs, Registry, Root};
use harness::tools::Surface;
use harness::{HarnessRun, Seed};
use support::{Scripted, drain_transcript, grant, input, text_reply, tool_call_reply};

/// A directory of this test's own, removed when the test ends.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("faber-harness-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a temp directory");
        TempDir(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Binds one local target as `build` and projects the standard surface over
/// it. Bindings are assembled by the consumer, before the run — nothing the
/// model calls can add one.
fn surface_over(root: &std::path::Path) -> Arc<Surface> {
    let blobs: Arc<dyn Blobs> = Arc::new(MemoryBlobs::new());
    let machine = tokio::runtime::Runtime::new()
        .expect("a runtime")
        .block_on(LocalTarget::bind(
            "build",
            Root::new(root.to_str().expect("a utf-8 temp path")).expect("a root"),
            Arc::clone(&blobs),
        ))
        .expect("binding a local target");

    let mut registry = Registry::new();
    registry
        .bind("build", Arc::new(machine))
        .expect("a fresh label");
    Arc::new(Surface::new(Arc::new(registry), blobs))
}

#[test]
fn the_shipped_harness_runs_a_granted_tool_without_defining_one() {
    let dir = TempDir::new("granted");
    std::fs::write(dir.0.join("hello.txt"), "from the environment\n").expect("a fixture file");

    let client = Arc::new(Scripted::sequence(vec![
        tool_call_reply(
            "call_1",
            "read",
            &format!(
                r#"{{"execute_in":"build","path":"{}"}}"#,
                dir.0.join("hello.txt").display()
            ),
        ),
        text_reply("the file says: from the environment"),
    ]));

    let surface = surface_over(&dir.0);
    let mut granted = grant(client);
    granted.tools = Surface::definitions();
    granted.tool_invoker = Some(Arc::clone(&surface).invoker());

    let mut run = HarnessRun::start(
        harness::CONVERSATIONAL.to_owned(),
        input("what does hello.txt say?"),
        granted,
        Seed::default(),
    );
    let events = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    let call = events
        .iter()
        .find(|event| event["type"] == "tool_call")
        .expect("the harness invoked the tool the model asked for");
    assert_eq!(call["name"], "read");

    let result = events
        .iter()
        .find(|event| event["type"] == "tool_result")
        .expect("and fed the result back");
    assert_eq!(result["isError"], false);
    // Echoed target and path, and the file's actual bytes.
    assert!(
        result["content"]
            .as_str()
            .expect("a rendered result")
            .contains("build:/hello.txt")
    );
    assert!(
        result["content"]
            .as_str()
            .expect("a rendered result")
            .contains("from the environment")
    );
}

#[test]
fn a_command_that_exits_nonzero_comes_back_as_a_result_the_model_can_act_on() {
    let dir = TempDir::new("nonzero");

    let client = Arc::new(Scripted::sequence(vec![
        tool_call_reply(
            "call_1",
            "exec",
            r#"{"execute_in":"build","command":"echo nope >&2; exit 2"}"#,
        ),
        text_reply("it exited 2"),
    ]));

    let surface = surface_over(&dir.0);
    let mut granted = grant(client);
    granted.tools = Surface::definitions();
    granted.tool_invoker = Some(Arc::clone(&surface).invoker());

    let mut run = HarnessRun::start(
        harness::CONVERSATIONAL.to_owned(),
        input("run it"),
        granted,
        Seed::default(),
    );
    let events = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    let result = events
        .iter()
        .find(|event| event["type"] == "tool_result")
        .expect("the command ran");
    // The command ran and reported. There is no allowlist of commands whose
    // nonzero exit is forgiven, because failure and result never shared a
    // representation in the first place.
    assert_eq!(result["isError"], false, "{:?}", result);
    let content = result["content"].as_str().expect("a rendered result");
    assert!(content.contains("exit 2"), "{content}");
    assert!(content.contains("nope"), "{content}");
}

#[test]
fn a_call_against_an_unbound_label_is_denied_rather_than_answered_empty() {
    let dir = TempDir::new("unbound");

    let client = Arc::new(Scripted::sequence(vec![
        tool_call_reply("call_1", "exec", r#"{"execute_in":"staging","command":"true"}"#),
        text_reply("staging is not bound"),
    ]));

    let surface = surface_over(&dir.0);
    let mut granted = grant(client);
    granted.tools = Surface::definitions();
    granted.tool_invoker = Some(Arc::clone(&surface).invoker());

    let mut run = HarnessRun::start(
        harness::CONVERSATIONAL.to_owned(),
        input("run it on staging"),
        granted,
        Seed::default(),
    );
    let events = drain_transcript(&mut run);
    support::finished(run, "run must finish");

    let result = events
        .iter()
        .find(|event| event["type"] == "tool_result")
        .expect("the call was answered");
    assert_eq!(result["isError"], true);
    assert!(
        result["content"]
            .as_str()
            .expect("a rendered denial")
            .contains("not_bound")
    );
}

#[test]
fn the_tool_loop_commits_the_whole_exchange_and_not_just_its_last_turn() {
    let dir = TempDir::new("lineage");

    let client = Arc::new(Scripted::sequence(vec![
        tool_call_reply("call_1", "bound_environments", "{}"),
        text_reply("one environment is bound"),
    ]));

    let surface = surface_over(&dir.0);
    let mut granted = grant(client);
    granted.tools = Surface::definitions();
    granted.tool_invoker = Some(Arc::clone(&surface).invoker());

    let mut run = HarnessRun::start(
        harness::CONVERSATIONAL.to_owned(),
        input("what can you reach?"),
        granted,
        Seed::default(),
    );
    let _ = drain_transcript(&mut run);
    let outcome = support::finished(run, "run must finish");

    // Only the last call is committed, and that is enough: each call carries
    // every earlier turn by value, so the last one's turn list is the whole
    // exchange. Input, tool_use, tool_result, and the final answer.
    let roles: Vec<_> = outcome
        .committed
        .messages
        .iter()
        .map(|message| format!("{:?}", message.role))
        .collect();
    assert_eq!(roles.len(), 4, "{roles:?}");
    assert!(outcome.committed_frame.is_some());
}
