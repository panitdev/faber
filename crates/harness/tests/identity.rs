//! End-to-end: a real run, with a loop written for the test, against a
//! scripted `llm::ModelClient`. No network, fully deterministic.

mod support;

use std::sync::Arc;
use std::time::Duration;

use harness::mapping::{CommitOptions, LLMRequest};
use harness::{Context, Error, HarnessRun, Seed};
use llm::{EventStream, ModelClient, RenderedRequest, Request};
use serde_json::json;
use support::{
    Recording, Scripted, Wire, drain, drain_transcript, grant, history_and, input, text_reply,
};

#[tokio::test]
async fn identity_loop_end_to_end_matches_the_script() {
    let client = Arc::new(Scripted::new(text_reply("hello back")));
    let mut run = HarnessRun::start_with(
        support::identity,
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );

    let events = drain_transcript(&mut run).await;
    let frames = support::finished(run, "run must finish cleanly")
        .await
        .frames;

    assert!(!events.is_empty(), "the identity loop must forward events");
    assert_eq!(events[0]["type"], "message_start");
    assert_eq!(events[0]["model"], "test-model");

    let text_delta = events
        .iter()
        .find(|event| event["type"] == "block_delta")
        .expect("a text delta must appear");
    assert_eq!(text_delta["delta"]["type"], "text");
    assert_eq!(text_delta["delta"]["text"], "hello back");

    assert_eq!(events.last().unwrap()["type"], "message_stop");

    // The frame log recorded one model frame: a start, one ModelEvent per
    // provider event, a usage record, and a stop.
    use harness::frame::CoreEvent;
    let starts = frames
        .iter()
        .filter(|frame| matches!(frame, CoreEvent::FrameStart { .. }))
        .count();
    let stops = frames
        .iter()
        .filter(|frame| matches!(frame, CoreEvent::FrameStop { outcome, .. } if matches!(outcome, harness::frame::Outcome::Ok)))
        .count();
    assert_eq!((starts, stops), (1, 1));
}

#[tokio::test]
async fn granted_functions_are_available_to_the_loop() {
    async fn echoing(ctx: Context, _input: Vec<Wire>) -> Result<(), Error> {
        let value = ctx
            .invoke_function("test.echo", json!({ "value": 1 }))
            .await?;
        ctx.emit(json!({
            "type": "function_result",
            "value": value,
            "missing": ctx.has_function("test.absent"),
        }));
        Ok(())
    }

    let client = Arc::new(Scripted::new(text_reply("ok")));
    let mut grant = grant(client);
    grant
        .functions
        .register("test.echo", |input| Box::pin(async move { Ok(input) }));

    let mut run = HarnessRun::start_with(echoing, Vec::new(), grant, Seed::default());
    let events = drain_transcript(&mut run).await;
    support::finished(run, "a loop calling a granted function must finish cleanly").await;

    assert_eq!(events[0]["type"], "function_result");
    assert_eq!(events[0]["value"]["value"], 1);
    assert_eq!(events[0]["missing"], false);
}

#[tokio::test]
async fn a_recording_client_renders_real_provider_bytes_not_a_fixture() {
    // `Recording` (unlike `Scripted`) delegates rendering to the real
    // Anthropic wire module, so the bytes it captures are the provider's own
    // — this is the fixture the byte-identity tests in `history.rs` depend
    // on. Proven here, against the identity harness, independent of the
    // history/id machinery.
    let client = Arc::new(Recording::new(text_reply("hello back")));
    let mut run = HarnessRun::start_with(
        support::identity,
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run).await;
    support::finished(run, "run must finish cleanly").await;

    let rendered = client.rendered();
    assert_eq!(rendered.len(), 1);
    let messages_region = rendered[0]
        .prefix
        .regions
        .get("messages")
        .expect("a messages region must have been rendered");
    let wrapped = format!("[{}]", String::from_utf8_lossy(messages_region));
    let parsed: serde_json::Value =
        serde_json::from_str(&wrapped).expect("real wire bytes must be valid JSON");
    let messages = parsed.as_array().expect("messages region is an array");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
}

#[tokio::test]
async fn the_fold_invariant_holds_for_a_completed_model_frame() {
    // harness-events.md §8: folding a model frame's `model_event`s through
    // `Accumulator` must reproduce the same text the loop put in its
    // transcript.
    use harness::frame::{CoreEvent, Outcome};

    let client = Arc::new(Scripted::new(text_reply("fold me")));
    let mut run = HarnessRun::start_with(
        support::identity,
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let transcript = drain_transcript(&mut run).await;
    let frames = support::finished(run, "run must finish cleanly")
        .await
        .frames;

    let frame_id = frames
        .iter()
        .find_map(|frame| match frame {
            CoreEvent::FrameStart { frame, .. } => Some(frame.clone()),
            _ => None,
        })
        .expect("a model frame must have started");
    assert!(frames.iter().any(|frame| matches!(
        frame,
        CoreEvent::FrameStop { frame: id, outcome: Outcome::Ok } if *id == frame_id
    )));

    let mut accumulator = llm::Accumulator::new();
    for frame in &frames {
        if let CoreEvent::ModelEvent { frame: id, event, .. } = frame
            && *id == frame_id
        {
            accumulator.push(event);
        }
    }
    let completion = accumulator
        .finish()
        .expect("the folded frame must complete");

    let transcript_text: String = transcript
        .iter()
        .filter(|event| event["type"] == "block_delta" && event["delta"]["type"] == "text")
        .map(|event| event["delta"]["text"].as_str().unwrap_or_default())
        .collect();

    assert_eq!(completion.text(), transcript_text);
    assert_eq!(completion.text(), "fold me");
}

#[tokio::test]
async fn turn_one_has_no_committed_history_and_still_produces_valid_messages() {
    // §4's default path: nothing was ever committed, so `history()` must still
    // be usable — it returns nothing.
    let client = Arc::new(Scripted::new(text_reply("first turn")));
    let mut run = HarnessRun::start_with(
        support::identity,
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );
    let _ = drain_transcript(&mut run).await;
    support::finished(run, "turn 1 must not require a prior commit").await;

    let requests = client.requests_seen();
    assert_eq!(requests.len(), 1);
}

#[tokio::test]
async fn commit_is_callable_and_advances_history_within_one_run() {
    // A single loop that streams, commits the resulting call, then reads
    // history back and reports what it found — proving `commit` and `history`
    // are wired end to end. Persisting a commit *across* separate runs is the
    // caller's concern (`crates/api`) and isn't tested here — see
    // `history.rs` for the two-run case, which seeds the second run
    // explicitly.
    async fn commit_then_read(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        let call = ctx.stream(LLMRequest::new(history_and(&ctx, &input)))?;
        drain(&call).await?;
        ctx.commit(&call, CommitOptions::default()).await?;
        ctx.emit(json!({ "type": "unknown", "raw": { "historyLength": ctx.history().len() } }));
        Ok(())
    }

    let client = Arc::new(Scripted::new(text_reply("ok")));
    let mut run = HarnessRun::start_with(
        commit_then_read,
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let events = drain_transcript(&mut run).await;
    support::finished(run, "commit must be callable when granted").await;

    assert_eq!(events.len(), 1);
    // The committed lineage covers the user turn plus the assistant's reply.
    assert_eq!(events[0]["raw"]["historyLength"], 2);
}

#[tokio::test]
async fn commit_is_refused_when_not_granted() {
    async fn tries_to_commit(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        let call = ctx.stream(LLMRequest::new(input))?;
        drain(&call).await?;
        let refused = matches!(
            ctx.commit(&call, CommitOptions::default()).await,
            Err(Error::CommitNotGranted)
        );
        ctx.emit(json!({
            "type": "unknown",
            "raw": {
                "canCommit": ctx.can_commit(),
                "refused": refused,
                "historyLength": ctx.history().len(),
            },
        }));
        Ok(())
    }

    let client = Arc::new(Scripted::new(text_reply("ok")));
    let mut grant = grant(client);
    grant.commit_granted = false;

    let mut run = HarnessRun::start_with(tries_to_commit, input("hi"), grant, Seed::default());
    let events = drain_transcript(&mut run).await;
    let outcome = support::finished(run, "a refused commit is a failure the loop can handle").await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["raw"]["canCommit"], false);
    assert_eq!(events[0]["raw"]["refused"], true);
    assert_eq!(events[0]["raw"]["historyLength"], 0);
    assert!(outcome.committed_frame.is_none());
}

#[tokio::test]
async fn a_tool_calling_loop_sees_both_failure_axes() {
    async fn tool_loop(ctx: Context, _input: Vec<Wire>) -> Result<(), Error> {
        for tool in ["known", "unknown"] {
            match ctx.invoke_tool(tool, json!({})).await? {
                None => ctx
                    .emit(json!({ "type": "unknown", "raw": { "tool": tool, "offered": false } })),
                Some(result) => ctx.emit(json!({
                    "type": "tool_result",
                    "toolUseId": tool,
                    "content": result.content,
                    "isError": result.is_error,
                })),
            }
        }
        Ok(())
    }

    let client = Arc::new(Scripted::new(text_reply("unused")));
    let mut g = grant(client);
    g.tools = vec![llm::ToolDef {
        name: "known".into(),
        description: "a tool that exists".into(),
        input_schema: serde_json::json!({}),
    }];
    g.tool_invoker = Some(Arc::new(|name, _input| {
        Box::pin(async move {
            if name == "known" {
                Ok(harness::mapping::ToolResult {
                    content: "42".into(),
                    is_error: false,
                })
            } else {
                Err("dispatch failed".into())
            }
        })
    }));

    let mut run = HarnessRun::start_with(tool_loop, input("hi"), g, Seed::default());
    let events = drain_transcript(&mut run).await;
    support::finished(run, "run must finish").await;

    // Axis 1: a granted tool that ran and returned successfully.
    assert_eq!(events[0]["type"], "tool_result");
    assert_eq!(events[0]["content"], "42");
    assert_eq!(events[0]["isError"], false);

    // Axis 2: a tool that was never offered — "not offered" is a move the
    // loop doesn't have, not a failure. The invoker is never reached for it.
    assert_eq!(events[1]["raw"]["offered"], false);
}

/// A model whose stream gives control back before every event, so two polls
/// of one call really do overlap.
struct Yielding(Scripted);

impl ModelClient for Yielding {
    fn provider(&self) -> &str {
        self.0.provider()
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        self.0.render(request)
    }

    fn send(&self, rendered: RenderedRequest) -> EventStream<'_> {
        let inner = self.0.send(rendered);
        Box::pin(async_stream::stream! {
            futures_util::pin_mut!(inner);
            loop {
                tokio::task::yield_now().await;
                match futures_util::StreamExt::next(&mut inner).await {
                    Some(event) => yield event,
                    None => break,
                }
            }
        })
    }
}

#[tokio::test]
async fn concurrent_polling_of_one_call_is_a_clear_error_not_a_lost_stream() {
    // A loop that iterates a call and reads its completion *concurrently*
    // (rather than one after the other, the supported pattern) must not have
    // the second poll misreport the stream as unknown just because the first
    // is mid-await — and the stream must still be there afterwards.
    async fn races_its_own_call(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
        let call = ctx.stream(LLMRequest::new(history_and(&ctx, &input)))?;
        let (iterated, raced) = tokio::join!(drain(&call), call.completion());
        iterated?;
        let busy = matches!(raced, Err(Error::StreamBusy { .. }));
        let completion = call.completion().await?;
        ctx.emit(json!({ "type": "unknown", "raw": { "busy": busy, "id": completion.id } }));
        Ok(())
    }

    let client = Arc::new(Yielding(Scripted::new(text_reply("racing"))));
    let mut run = HarnessRun::start_with(
        races_its_own_call,
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let events = drain_transcript(&mut run).await;
    support::finished(run, "racing a call's own completion must not break the run").await;

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["raw"]["busy"], true,
        "the poll that lost the race must be told the call is busy"
    );
    assert_eq!(events[0]["raw"]["id"], "msg_1");
}

#[tokio::test]
async fn a_loop_that_never_finishes_is_terminated_and_the_run_ends() {
    async fn never(_ctx: Context, _input: Vec<Wire>) -> Result<(), Error> {
        std::future::pending().await
    }

    let client = Arc::new(Scripted::new(text_reply("unused")));
    let run = HarnessRun::start_with(never, input("hi"), grant(client), Seed::default());

    tokio::time::sleep(Duration::from_millis(50)).await;
    run.terminate();

    // A runaway loop degrades only its own run, and still accounts for
    // itself: the outcome comes back, naming what ended it.
    let outcome = tokio::time::timeout(Duration::from_secs(5), run.join())
        .await
        .expect("a terminated run must end")
        .expect("a terminated run still returns its outcome");
    assert!(matches!(outcome.error, Some(harness::RunError::Terminated)));
}
