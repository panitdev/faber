//! End-to-end: what a harness sees when the upstream provider fails.
//!
//! The contract under test is `types.d.ts`'s `HarnessError`: failures are
//! values carrying `kind`/`transient`/`status`/`requestId` as properties —
//! never message strings to regex — so every test here asserts on those
//! properties twice: once as the harness itself saw them (yielded through the
//! transcript), and once as Core recorded them (the frame log's terminal
//! `FrameStop`).
//!
//! The provider is always a mock. [`UpstreamMock`] replays scripted event
//! sequences (including terminal `Err`s) per model call, and [`RefusedPort`]
//! manufactures a genuine `llm::Error::Transport` by dialling a closed
//! loopback port — connection-refused, the same transient family as a connect
//! timeout. Nothing here leaves loopback. Every failure is reached through a
//! real isolate running a real harness.

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use harness::frame::{CoreEvent, Outcome};
use harness::mapping::HarnessErrorInfo;
use harness::{HarnessRun, Seed};
use llm::{
    BlockStart, Delta, Event, EventStream, ModelClient, RenderedRequest, RenderedSpan, Request,
    StopReason, UsageDelta,
};
use serde_json::{Value, json};
use support::{grant, input, text_reply};

// ---------------------------------------------------------------------------
// Upstream mocks
// ---------------------------------------------------------------------------

/// A scripted upstream provider: each model call consumes one script, in
/// order. A script is the exact event sequence one `send` yields, including a
/// terminal `Err` for upstream failures. The last script is deliberately NOT
/// repeated — every test makes exactly as many calls as it scripts, so
/// running dry is a test bug, surfaced loudly.
struct UpstreamMock {
    scripts: Mutex<VecDeque<Vec<llm::Result<Event>>>>,
    seen: Mutex<Vec<Request>>,
    render_fail: Option<String>,
}

impl UpstreamMock {
    fn new(scripts: Vec<Vec<llm::Result<Event>>>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            seen: Mutex::new(Vec::new()),
            render_fail: None,
        }
    }

    /// A mock whose `render` itself fails — the wire bytes could never be
    /// built, so the call is refused at `open`, before any frame exists.
    fn render_failing(message: &str) -> Self {
        Self {
            scripts: Mutex::new(VecDeque::new()),
            seen: Mutex::new(Vec::new()),
            render_fail: Some(message.into()),
        }
    }

    fn ok(events: Vec<Event>) -> Vec<llm::Result<Event>> {
        events.into_iter().map(Ok).collect()
    }

    fn fail(error: llm::Error) -> Vec<llm::Result<Event>> {
        vec![Err(error)]
    }

    fn mid_stream(prefix: Vec<Event>, error: llm::Error) -> Vec<llm::Result<Event>> {
        prefix.into_iter().map(Ok).chain(std::iter::once(Err(error))).collect()
    }

    fn requests_seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

impl ModelClient for UpstreamMock {
    fn provider(&self) -> &str {
        "mock-upstream"
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        if let Some(message) = &self.render_fail {
            return Err(llm::Error::Decode(message.clone()));
        }
        self.seen.lock().unwrap().push(request.clone());
        Ok(RenderedRequest {
            body: Vec::new(),
            prefix: RenderedSpan {
                provider: "mock-upstream".into(),
                model: request.model.clone(),
                reasoning: request
                    .reasoning_history
                    .unwrap_or(llm::ReasoningHistory::Full),
                regions: Default::default(),
            },
        })
    }

    fn send(&self, _rendered: RenderedRequest) -> EventStream<'_> {
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("more model calls than scripted upstream responses");
        Box::pin(futures_util::stream::iter(script.into_iter()))
    }
}

/// Manufactures a genuine `llm::Error::Transport` without leaving loopback:
/// port 1 is closed, so the dial fails fast with connection-refused, which
/// `llm::Error::is_transient` reads as retryable — the same family as a
/// connect timeout.
#[derive(Default)]
struct RefusedPort {
    seen: Mutex<Vec<Request>>,
}

impl RefusedPort {
    fn requests_seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

impl ModelClient for RefusedPort {
    fn provider(&self) -> &str {
        "mock-upstream"
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        self.seen.lock().unwrap().push(request.clone());
        Ok(RenderedRequest {
            body: Vec::new(),
            prefix: RenderedSpan {
                provider: "mock-upstream".into(),
                model: request.model.clone(),
                reasoning: request
                    .reasoning_history
                    .unwrap_or(llm::ReasoningHistory::Full),
                regions: Default::default(),
            },
        })
    }

    fn send(&self, _rendered: RenderedRequest) -> EventStream<'_> {
        Box::pin(async_stream::stream! {
            let error = reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("test client builds")
                .get("http://127.0.0.1:1/")
                .send()
                .await
                .expect_err("loopback port 1 must refuse the connection");
            yield Err::<Event, _>(llm::Error::Transport(error));
        })
    }
}

fn api_error(status: u16, kind: &str, message: &str, request_id: Option<&str>) -> llm::Error {
    llm::Error::Api {
        status: Some(status),
        kind: Some(kind.into()),
        message: message.into(),
        request_id: request_id.map(str::to_string),
    }
}

/// A call that starts answering and then breaks: the shape every mid-stream
/// failure takes, whatever its cause.
fn streaming_text_prefix(text: &str) -> Vec<Event> {
    vec![
        Event::MessageStart {
            id: "msg_partial".into(),
            model: "test-model".into(),
            usage: UsageDelta {
                input_tokens: Some(5),
                ..UsageDelta::default()
            },
        },
        Event::BlockStart {
            index: 0,
            block: BlockStart::Text,
        },
        Event::BlockDelta {
            index: 0,
            delta: Delta::Text {
                content: text.into(),
            },
        },
    ]
}

/// A tool call whose streamed arguments end truncated: the stream itself
/// closes cleanly, and only folding the events fails — an internal invariant
/// violation, not anything the provider reported.
fn truncated_tool_call() -> Vec<Event> {
    vec![
        Event::MessageStart {
            id: "msg_tool".into(),
            model: "test-model".into(),
            usage: UsageDelta::default(),
        },
        Event::BlockStart {
            index: 0,
            block: BlockStart::ToolUse {
                id: "t1".into(),
                name: "read".into(),
            },
        },
        Event::BlockDelta {
            index: 0,
            delta: Delta::ToolInputJson {
                content: "{\"path\":".into(),
            },
        },
        Event::BlockStop { index: 0 },
        Event::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            stop_details: None,
            usage: UsageDelta::default(),
        },
        Event::MessageStop,
    ]
}

// ---------------------------------------------------------------------------
// Harness fixtures and assertion helpers
// ---------------------------------------------------------------------------

/// Streams one call, yielding whatever arrives, and reports a caught failure
/// as its properties — never its message. `transient` goes through
/// `String(...)` so the assertion holds whether the boundary hands JS a
/// boolean or its string form; `status`/`requestId` normalize the op layer's
/// empty-string-for-absent to `null`.
const CATCH_ONE: &str = r#"
export default {
  execute: async function* (ctx, input) {
    try {
      const call = ctx.llm.stream({ messages: [...input] });
      for await (const event of call) {
        yield event;
      }
      yield { type: "done" };
    } catch (error) {
      yield {
        type: "caught",
        kind: error.kind ?? null,
        transient: String(error.transient),
        status: error.status === "" ? null : (error.status ?? null),
        requestId: error.requestId === "" ? null : (error.requestId ?? null),
      };
    }
  }
};
"#;

/// Retries a transient failure once, then answers. Rethrows anything that is
/// not transient — the run-level error that leaves behind is the assertion
/// for the non-retryable case.
const RETRY_ONCE: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const messages = [...input];
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        const call = ctx.llm.stream({ messages });
        for await (const event of call) {
          yield event;
        }
        yield { type: "ok", attempt };
        return;
      } catch (error) {
        yield {
          type: "caught",
          attempt,
          kind: error.kind ?? null,
          transient: String(error.transient),
        };
        if (String(error.transient) !== "true") {
          throw error;
        }
      }
    }
  }
};
"#;

/// Drains a call that is expected to break, reports the failure, then shows
/// the two commit paths: a plain `commit` must refuse the truncation, and a
/// partial commit must adopt what survived.
const COMMIT_PARTIAL: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const call = ctx.llm.stream({ messages: [...input] });
    let kind = null;
    try {
      for await (const event of call) {
        yield event;
      }
    } catch (error) {
      kind = error.kind ?? null;
    }
    let commitKind = null;
    try {
      await ctx.commit(call);
    } catch (error) {
      commitKind = error.kind ?? null;
    }
    await ctx.commit(call, { partial: true });
    yield {
      type: "summary",
      kind,
      commitKind,
      historyLength: ctx.history.read().length,
    };
  }
};
"#;

fn caught(events: &[Value]) -> &Value {
    events
        .iter()
        .find(|event| event["type"] == "caught")
        .expect("the harness must yield what it caught")
}

fn failed_frames(frames: &[CoreEvent]) -> Vec<&HarnessErrorInfo> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            CoreEvent::FrameStop {
                outcome: Outcome::Failed { error },
                ..
            } => Some(error),
            _ => None,
        })
        .collect()
}

fn frame_starts(frames: &[CoreEvent]) -> usize {
    frames
        .iter()
        .filter(|frame| matches!(frame, CoreEvent::FrameStart { .. }))
        .count()
}

fn clean_stops(frames: &[CoreEvent]) -> usize {
    frames
        .iter()
        .filter(|frame| {
            matches!(
                frame,
                CoreEvent::FrameStop {
                    outcome: Outcome::Ok,
                    ..
                }
            )
        })
        .count()
}

fn run_to_completion(
    harness_source: &str,
    client: Arc<dyn ModelClient>,
    why: &str,
) -> (Vec<Value>, harness::RunOutcome) {
    let mut run = HarnessRun::start(
        harness_source.to_string(),
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let events = support::drain_transcript(&mut run);
    let outcome = support::finished(run, why);
    (events, outcome)
}

// ---------------------------------------------------------------------------
// Upstream failures: timeout, bad gateway, rate limit, mid-stream break
// ---------------------------------------------------------------------------

#[test]
fn an_upstream_timeout_surfaces_as_a_transient_transport_failure() {
    let client = Arc::new(RefusedPort::default());
    let (events, outcome) = run_to_completion(
        CATCH_ONE,
        client.clone(),
        "a transport failure is catchable in JS, not a broken run",
    );

    let caught = caught(&events);
    assert_eq!(caught["kind"], json!("transport"));
    assert_eq!(caught["transient"], json!("true"));

    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1, "the call must stop failed exactly once");
    assert_eq!(failed[0].kind, "transport");
    assert!(failed[0].transient, "a refused dial must read as retryable");
    assert_eq!(
        client.requests_seen().len(),
        1,
        "the call was rendered and dispatched once"
    );
}

#[test]
fn a_bad_gateway_is_transient_and_the_harness_retry_succeeds() {
    let client = Arc::new(UpstreamMock::new(vec![
        UpstreamMock::fail(api_error(502, "api_error", "Bad Gateway", None)),
        UpstreamMock::ok(text_reply("recovered")),
    ]));
    let (events, outcome) = run_to_completion(
        RETRY_ONCE,
        client.clone(),
        "a retried 502 must still finish cleanly",
    );

    let caught: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "caught")
        .collect();
    assert_eq!(caught.len(), 1, "only the first attempt fails");
    assert_eq!(caught[0]["kind"], json!("api"));
    assert_eq!(caught[0]["transient"], json!("true"));

    let ok = events
        .iter()
        .find(|event| event["type"] == "ok")
        .expect("the retry must answer");
    assert_eq!(ok["attempt"], json!(1));
    assert_eq!(
        client.requests_seen().len(),
        2,
        "both attempts reach the provider"
    );

    assert_eq!(frame_starts(&outcome.frames), 2);
    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "api");
    assert!(failed[0].transient);
    assert_eq!(failed[0].status, Some(502));
    assert_eq!(
        clean_stops(&outcome.frames),
        1,
        "the retry completes the second frame"
    );
}

#[test]
fn a_rate_limit_carries_its_status_and_request_id_to_the_harness() {
    let client = Arc::new(UpstreamMock::new(vec![UpstreamMock::fail(api_error(
        429,
        "rate_limit_error",
        "Rate limit reached for the model",
        Some("req_rl_1"),
    ))]));
    let (events, outcome) = run_to_completion(
        CATCH_ONE,
        client.clone(),
        "a rate limit is catchable in JS, not a broken run",
    );

    let caught = caught(&events);
    assert_eq!(caught["kind"], json!("api"));
    assert_eq!(caught["transient"], json!("true"));
    assert_eq!(caught["status"], json!(429));
    assert_eq!(caught["requestId"], json!("req_rl_1"));

    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "api");
    assert!(failed[0].transient);
    assert_eq!(failed[0].status, Some(429));
    assert_eq!(failed[0].request_id.as_deref(), Some("req_rl_1"));
}

#[test]
fn a_mid_stream_break_keeps_what_arrived_and_fails_the_frame() {
    let client = Arc::new(UpstreamMock::new(vec![UpstreamMock::mid_stream(
        streaming_text_prefix("partial"),
        api_error(
            503,
            "overloaded_error",
            "upstream closed the connection mid-stream",
            None,
        ),
    )]));
    let (events, outcome) = run_to_completion(
        COMMIT_PARTIAL,
        client.clone(),
        "a mid-stream break must still finish once the harness handles it",
    );

    assert!(
        events.iter().any(|event| event["type"] == "block_delta"
            && event["delta"]["text"] == json!("partial")),
        "text that arrived before the break must survive it"
    );

    let summary = events
        .iter()
        .find(|event| event["type"] == "summary")
        .expect("the harness must summarize the broken call");
    assert_eq!(summary["kind"], json!("api"));
    assert_eq!(
        summary["commitKind"], json!("incomplete_completion"),
        "a plain commit must refuse the truncation"
    );
    assert_eq!(
        summary["historyLength"], json!(2),
        "a partial commit adopts the input plus what survived"
    );

    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "api");
    assert_eq!(failed[0].status, Some(503));
}

#[test]
fn a_non_transient_failure_is_not_retried() {
    let client = Arc::new(UpstreamMock::new(vec![UpstreamMock::fail(api_error(
        400,
        "invalid_request_error",
        "bad request",
        None,
    ))]));

    let mut run = HarnessRun::start(
        RETRY_ONCE.to_string(),
        input("hi"),
        grant(client.clone()),
        Seed::default(),
    );
    let events = support::drain_transcript(&mut run);
    let outcome = run
        .join()
        .expect("a rethrown failure still returns its frame log");

    let caught: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "caught")
        .collect();
    assert_eq!(caught.len(), 1, "the harness sees the failure once");
    assert_eq!(caught[0]["kind"], json!("api"));
    assert_eq!(
        caught[0]["transient"],
        json!("false"),
        "a 400 is not retryable"
    );
    assert_eq!(
        client.requests_seen().len(),
        1,
        "a non-transient failure must not be answered with another call"
    );
    assert!(
        outcome.error.is_some(),
        "the rethrown failure must end the run in error"
    );
    assert_eq!(failed_frames(&outcome.frames).len(), 1);
}

// ---------------------------------------------------------------------------
// Internal crashes: render refusal, unfoldable stream, uncaught harness throw
// ---------------------------------------------------------------------------

#[test]
fn a_render_failure_rejects_at_open_before_any_frame_exists() {
    let client = Arc::new(UpstreamMock::render_failing("definitely not wire bytes"));
    let (events, outcome) = run_to_completion(
        CATCH_ONE,
        client.clone(),
        "a render failure is catchable in JS, not a broken run",
    );

    let caught = caught(&events);
    assert_eq!(caught["kind"], json!("decode"));
    assert_eq!(caught["transient"], json!("false"));
    assert_eq!(caught["status"], Value::Null);

    assert_eq!(
        frame_starts(&outcome.frames),
        0,
        "a call refused at open must log no frame at all"
    );
    assert!(
        client.requests_seen().is_empty(),
        "nothing was rendered, so nothing could have been sent"
    );
}

#[test]
fn a_truncated_tool_call_fails_as_tool_input_not_as_an_answer() {
    let client = Arc::new(UpstreamMock::new(vec![UpstreamMock::ok(
        truncated_tool_call(),
    )]));
    let (events, outcome) = run_to_completion(
        CATCH_ONE,
        client.clone(),
        "an unfoldable stream is catchable in JS, not a broken run",
    );

    let caught = caught(&events);
    assert_eq!(caught["kind"], json!("tool_input"));
    assert_eq!(caught["transient"], json!("false"));

    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "tool_input");
    assert!(
        !failed[0].transient,
        "malformed arguments will not repair themselves on retry"
    );
    assert!(
        !events.iter().any(|event| event["type"] == "done"),
        "the call never completed, so the harness must not report success"
    );
}

#[test]
fn an_empty_stream_is_a_failure_not_an_empty_answer() {
    let client = Arc::new(UpstreamMock::new(vec![UpstreamMock::ok(vec![])]));
    let (events, outcome) = run_to_completion(
        CATCH_ONE,
        client.clone(),
        "an empty stream is catchable in JS, not a broken run",
    );

    let caught = caught(&events);
    assert_eq!(caught["kind"], json!("empty_response"));

    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "empty_response");
}

#[test]
fn an_uncaught_harness_throw_keeps_the_frame_log() {
    const UNCAUGHT: &str = r#"
export default {
  execute: async function* (ctx, input) {
    const call = ctx.llm.stream({ messages: [...input] });
    for await (const event of call) {
      yield event;
    }
    throw new Error("boom inside the harness");
  }
};
"#;

    let client = Arc::new(UpstreamMock::new(vec![UpstreamMock::ok(text_reply(
        "hello back",
    ))]));
    let mut run = HarnessRun::start(
        UNCAUGHT.to_string(),
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let events = support::drain_transcript(&mut run);
    let outcome = run
        .join()
        .expect("a crashed run still returns its frame log");

    assert!(
        outcome.error.is_some(),
        "an uncaught throw must end the run in error"
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "block_delta"),
        "what streamed before the crash is still the conversation"
    );
    assert!(
        outcome
            .frames
            .iter()
            .any(|frame| matches!(frame, CoreEvent::ModelRequest { .. })),
        "the frame log keeps ground truth of what reached the provider"
    );
    assert!(
        outcome.committed_frame.is_none(),
        "a crashed run commits nothing, so there is no position to record"
    );
    assert!(
        outcome.committed.messages.is_empty(),
        "a crashed run must not advance the lineage"
    );
}
