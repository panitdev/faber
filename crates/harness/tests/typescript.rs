//! A TypeScript harness with a relative submodule import, exercising the
//! Deno-compatible resolution and TS->JS transpile path end-to-end.

use std::sync::Arc;

use harness::{Grant, HarnessRun, Seed};
use llm::anthropic::{Anthropic, Config};
use llm::{AdvancedOptions, ContentBlock, Message, Role};
use secrecy::SecretString;

fn grant(client: Arc<dyn llm::ModelClient>) -> Grant {
    Grant {
        client,
        model: llm::anthropic::DEFAULT_MODEL.to_string(),
        reasoning_history: None,
        advanced_options: AdvancedOptions::default(),
        tools: Vec::new(),
        tool_invoker: None,
        commit_granted: true,
        functions: harness::FunctionRegistry::new(),
        interrupt: None,
    }
}

fn input(text: &str) -> Vec<llm::Message> {
    vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
    }]
}

fn client() -> Arc<dyn llm::ModelClient> {
    Arc::new(Anthropic::new(Config::new(SecretString::from("fake-key"))).expect("client builds"))
}

#[tokio::test]
async fn a_typescript_harness_with_a_relative_import_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    // main.ts imports ./helper.ts and uses a typed helper value.
    std::fs::write(
        dir.path().join("main.ts"),
        r#"
import { prefix } from "./helper.ts";

export default {
  async *execute(ctx, input) {
    yield { type: "unknown", raw: { marker: prefix } };
  },
};
"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("helper.ts"),
        r#"
export const prefix: string = "typed-helper-ok";
"#,
    )
    .unwrap();

    let harness = harness::Harness::from_dir(dir.path()).expect("harness builds");
    let mut run = HarnessRun::start(harness, input("hi"), grant(client()), Seed::default());

    let mut transcript = Vec::new();
    while let Some(event) = run.transcript.recv().await {
        transcript.push(event);
    }
    let outcome = tokio::task::spawn_blocking(move || run.join())
        .await
        .expect("join task")
        .expect("run outcome");

    assert!(outcome.error.is_none(), "run error: {:?}", outcome.error);
    assert!(
        transcript.iter().any(|e| {
            e.get("type").and_then(|t| t.as_str()) == Some("unknown")
                && e["raw"]["marker"].as_str() == Some("typed-helper-ok")
        }),
        "transcript did not carry the typed helper value: {transcript:?}"
    );
}

#[tokio::test]
async fn an_https_import_to_a_host_outside_the_allowlist_is_refused_at_graph_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("main.js"),
        r#"
import "https://npmjs.com/package/entry.js";
export default {
  async *execute(ctx, input) {
    yield { type: "unknown", raw: {} };
  },
};
"#,
    )
    .unwrap();

    let harness = harness::Harness::from_dir(dir.path()).expect("harness builds");
    // The graph build fetches eagerly, so the refusal surfaces as a run
    // error. The non-allowlisted host is rejected before any fetch.
    let mut run = HarnessRun::start(harness, input("hi"), grant(client()), Seed::default());
    while run.transcript.recv().await.is_some() {}
    let outcome = tokio::task::spawn_blocking(move || run.join())
        .await
        .expect("join task")
        .expect("run outcome");
    assert!(
        outcome.error.is_some(),
        "an https import from a non-allowlisted host must be refused"
    );
}

#[tokio::test]
async fn an_absolute_import_outside_the_harness_directory_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("main.js"),
        r#"
import "/etc/passwd";
export default {
  async *execute(ctx, input) {
    yield { type: "unknown", raw: {} };
  },
};
"#,
    )
    .unwrap();

    let harness = harness::Harness::from_dir(dir.path()).expect("harness builds");
    let mut run = HarnessRun::start(harness, input("hi"), grant(client()), Seed::default());
    while run.transcript.recv().await.is_some() {}
    let outcome = tokio::task::spawn_blocking(move || run.join())
        .await
        .expect("join task")
        .expect("run outcome");

    assert!(
        outcome.error.is_some(),
        "an import outside the harness directory must be refused, not silently resolved"
    );
}
