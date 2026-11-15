//! One end-to-end test per surviving §7 row (`proposal.md`) — everything
//! `Context::stream` and `Context::commit` can refuse a request for, reached
//! through a real run rather than unit-tested against `validate::check`
//! directly.
//!
//! Not covered here:
//! - `scaffold_mismatch { component: "model" }` — unreachable within one run
//!   (one client, hence one model, per run); covered as a unit test in
//!   `crates/harness/src/validate.rs`.
//! - `tool_removal` — proposal §5 (mid-conversation tool changes) is
//!   deferred, so there is nothing yet that could orphan a removal.

mod support;

use std::sync::Arc;
use std::time::Duration;

use harness::mapping::{CommitOptions, Content, LLMRequest, Role, ToolDef, WireTurn};
use harness::{Context, Error, Grant, HarnessRun, Seed};
use serde_json::json;
use support::{
    Failing, Scripted, Wire, drain, drain_and_join_with_timeout, drain_transcript, grant, input,
    text_reply,
};

/// Defines a loop that attempts `$body` and reports the `kind` of whatever it
/// was refused with, or `null` if nothing refused it.
macro_rules! refused {
    ($name:ident, |$ctx:ident, $input:ident| $body:block) => {
        async fn $name($ctx: Context, $input: Vec<Wire>) -> Result<(), Error> {
            let attempt: Result<(), Error> = async {
                $body
                Ok(())
            }
            .await;
            let kind = attempt.err().and_then(|error| error.kind());
            $ctx.emit(json!({ "type": "unknown", "raw": { "kind": kind } }));
            Ok(())
        }
    };
}

/// Runs a loop and returns the `raw` of the single summary event every test
/// in this file emits.
async fn summary<F, Fut>(run: F, grant: Grant) -> serde_json::Value
where
    F: FnOnce(Context, Vec<Wire>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), Error>> + Send + 'static,
{
    let mut run = HarnessRun::start_with(run, input("hi"), grant, Seed::default());
    let mut events = drain_transcript(&mut run).await;
    support::finished(
        run,
        "the loop itself must not fail — every refusal here is handled in it",
    )
    .await;
    assert_eq!(events.len(), 1, "each test emits exactly one summary event");
    events.remove(0)["raw"].clone()
}

/// Streams and commits the input, so there is a lineage to refer to.
async fn commit_input(ctx: &Context, input: &[Wire]) -> Result<Vec<Wire>, Error> {
    let call = ctx.stream(LLMRequest::new(input.to_vec()))?;
    drain(&call).await?;
    ctx.commit(&call, CommitOptions::default()).await?;
    Ok(ctx.history())
}

fn two_scripts() -> Arc<Scripted> {
    Arc::new(Scripted::sequence(vec![
        text_reply("a"),
        text_reply("unused"),
    ]))
}

#[tokio::test]
async fn ref_not_prefix_on_reordered_ids() {
    refused!(reordered, |ctx, input| {
        let h = commit_input(&ctx, &input).await?;
        drain(&ctx.stream(LLMRequest::new([h[1].clone(), h[0].clone()]))?).await?;
    });
    let raw = summary(reordered, grant(two_scripts())).await;
    assert_eq!(raw["kind"], "ref_not_prefix");
}

#[tokio::test]
async fn ref_not_prefix_on_an_unknown_id() {
    refused!(unknown_id, |ctx, input| {
        let mut messages = vec![WireTurn::reference("m99")];
        messages.extend(input.into_iter().map(WireTurn::from));
        ctx.stream(LLMRequest {
            messages,
            ..LLMRequest::default()
        })?;
    });
    let raw = summary(
        unknown_id,
        grant(Arc::new(Scripted::new(text_reply("unused")))),
    )
    .await;
    assert_eq!(raw["kind"], "ref_not_prefix");
}

#[tokio::test]
async fn ref_content_mismatch_on_an_id_retaining_edit() {
    refused!(edited_copy, |ctx, input| {
        let h = commit_input(&ctx, &input).await?;
        // The idiomatic mistake: an edited copy that keeps the id.
        let edited = Wire {
            content: vec![Content::Text {
                text: "not what was stored".into(),
            }],
            ..h[1].clone()
        };
        drain(&ctx.stream(LLMRequest::new([h[0].clone(), edited]))?).await?;
    });
    let raw = summary(edited_copy, grant(two_scripts())).await;
    assert_eq!(raw["kind"], "ref_content_mismatch");
}

#[tokio::test]
async fn malformed_turns_on_an_orphaned_tool_result() {
    refused!(orphaned, |ctx, _input| {
        ctx.stream(LLMRequest::new([Wire {
            id: None,
            role: Role::User,
            content: vec![Content::ToolResult {
                tool_use_id: "t1".into(),
                content: "42".into(),
                is_error: false,
            }],
        }]))?;
    });
    let raw = summary(
        orphaned,
        grant(Arc::new(Scripted::new(text_reply("unused")))),
    )
    .await;
    assert_eq!(raw["kind"], "malformed_turns");
}

#[tokio::test]
async fn malformed_turns_on_consecutive_system_messages_after_the_head() {
    refused!(doubled_system, |ctx, _input| {
        ctx.stream(LLMRequest::new([
            Wire::text(Role::System, "sys"),
            Wire::text(Role::User, "hi"),
            Wire::text(Role::System, "mid1"),
            Wire::text(Role::System, "mid2"),
        ]))?;
    });
    let raw = summary(
        doubled_system,
        grant(Arc::new(Scripted::new(text_reply("unused")))),
    )
    .await;
    assert_eq!(raw["kind"], "malformed_turns");
}

#[tokio::test]
async fn scaffold_mismatch_on_reordered_tools() {
    refused!(reordered_tools, |ctx, input| {
        let h = commit_input(&ctx, &input).await?;
        let tool = |name: &str| ToolDef {
            name: name.into(),
            description: String::new(),
            input_schema: json!({}),
        };
        let mut messages = h;
        messages.extend(input);
        let call = ctx.stream(LLMRequest {
            tools: Some(vec![tool("b"), tool("a")]),
            ..LLMRequest::new(messages)
        })?;
        drain(&call).await?;
    });

    let mut g = grant(two_scripts());
    g.tools = vec![
        llm::ToolDef {
            name: "a".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        },
        llm::ToolDef {
            name: "b".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
        },
    ];
    let raw = summary(reordered_tools, g).await;
    assert_eq!(raw["kind"], "scaffold_mismatch");
}

#[tokio::test]
async fn scaffold_mismatch_on_a_partially_referenced_system_head() {
    refused!(half_a_head, |ctx, input| {
        let mut first = vec![
            Wire::text(Role::System, "sys1"),
            Wire::text(Role::System, "sys2"),
        ];
        first.extend(input.iter().cloned());
        let h = commit_input(&ctx, &first).await?;

        // References only the first of the two-message system head, then goes
        // straight to by-value content — the committed lineage's head no
        // longer matches what this request's own head resolves to.
        let mut messages = vec![WireTurn::reference(h[0].id.clone().unwrap())];
        messages.extend(input.into_iter().map(WireTurn::from));
        let call = ctx.stream(LLMRequest {
            messages,
            ..LLMRequest::default()
        })?;
        drain(&call).await?;
    });
    let raw = summary(half_a_head, grant(two_scripts())).await;
    assert_eq!(raw["kind"], "scaffold_mismatch");
}

#[tokio::test]
async fn incomplete_completion_is_refused_by_default_and_accepted_as_partial() {
    async fn commit_a_broken_call(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        let call = ctx.stream(LLMRequest::new(input))?;
        // The stream itself is expected to fail.
        let _ = drain(&call).await;

        let first_kind = ctx
            .commit(&call, CommitOptions::default())
            .await
            .err()
            .and_then(|error| error.kind());

        ctx.commit(&call, CommitOptions { partial: true }).await?;
        ctx.emit(json!({
            "type": "unknown",
            "raw": { "firstKind": first_kind, "historyLength": ctx.history().len() },
        }));
        Ok(())
    }

    let raw = summary(commit_a_broken_call, grant(Arc::new(Failing))).await;
    assert_eq!(raw["firstKind"], "incomplete_completion");
    assert_eq!(raw["historyLength"], 2);
}

/// Regression: committing a call drains it first, and that drain used to
/// ignore whatever error a poll returned and try again. For a call whose slot
/// had already been removed by an earlier commit — which is exactly the state
/// a second commit finds — the poll fails without ever awaiting, so the drain
/// spun forever instead of failing. It must instead be refused immediately,
/// promptly enough that the timeout below is never close.
#[tokio::test]
async fn committing_the_same_call_twice_is_a_clear_error_not_a_hang() {
    async fn double_commit(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        let call = ctx.stream(LLMRequest::new(input))?;
        drain(&call).await?;
        ctx.commit(&call, CommitOptions::default()).await?;

        let second = ctx.commit(&call, CommitOptions::default()).await;
        ctx.emit(json!({
            "type": "unknown",
            "raw": { "unknownStream": matches!(second, Err(Error::UnknownStream { .. })) },
        }));
        Ok(())
    }

    let client = Arc::new(Scripted::new(text_reply("ok")));
    let run = HarnessRun::start_with(double_commit, input("hi"), grant(client), Seed::default());
    let (mut events, _) = drain_and_join_with_timeout(run, Duration::from_secs(5)).await;

    assert_eq!(events.len(), 1);
    let raw = events.remove(0)["raw"].clone();
    assert_eq!(raw["unknownStream"], true);
}
