//! End-to-end: the *shipped* conversational harness against a mocked
//! upstream.
//!
//! [`upstream_failures`](super) proves the contract a harness is handed around
//! a failing call — using minimal harnesses written for the occasion. This
//! file proves the harness we actually ship against the same failures:
//! `harness::CONVERSATIONAL` runs its own `streamWithRetry` loop, its own tool
//! loop, and its own commit, so a mock upstream exercises those paths rather
//! than a fixture's.
//!
//! The provider is always a mock. A failure is one of three things, and each
//! is reached through a real isolate running real JavaScript:
//!
//! - a scripted `Err` on the stream (bad gateway, rate limit, mid-stream
//!   break),
//! - a genuine `llm::Error::Transport` manufactured without leaving loopback
//!   (a connect-refused dial or a real request timeout),
//! - an internal crash the provider never reported (a call refused at `open`
//!   because its bytes could not be rendered, or a stream that closes cleanly
//!   but cannot be folded into a completion).

mod support;

use std::collections::VecDeque;
use std::net::SocketAddr;
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
// The upstream mock
// ---------------------------------------------------------------------------

/// One scripted model call. `RenderFail` is consumed by `render` (the call
/// never reaches `send`); every other variant is consumed by `send`.
enum Reply {
    /// A call that answers and completes normally.
    Events(Vec<Event>),
    /// A call that fails before any event.
    Fail(llm::Error),
    /// A call that answers for a while and then breaks.
    MidStream(Vec<Event>, llm::Error),
    /// A real `llm::Error::Transport` from a request that times out against a
    /// loopback listener that accepts and never responds — a genuine timeout,
    /// not a synthesized one.
    Timeout,
    /// A real `llm::Error::Transport` from dialling a closed loopback port.
    Refused,
    /// `render` itself fails: the wire bytes could never be built.
    RenderFail(String),
}

/// A scripted upstream: each model call consumes one [`Reply`], in order. The
/// queue is deliberately not cycled — every test makes exactly as many calls
/// as it scripts, so running dry is a test bug surfaced loudly.
struct Upstream {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<Request>>,
    timeout_addr: SocketAddr,
    timeout: Duration,
}

impl Upstream {
    /// A mock against a fresh hanging listener, with a short timeout so a
    /// timeout test stays fast.
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            timeout_addr: spawn_hanging_listener(),
            timeout: Duration::from_millis(300),
        }
    }

    /// A mock whose very first `render` fails, and which never gets to `send`.
    fn render_failing(message: &str) -> Self {
        Self::new(vec![Reply::RenderFail(message.into())])
    }

    fn requests_seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

impl ModelClient for Upstream {
    fn provider(&self) -> &str {
        "mock-upstream"
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        {
            let mut replies = self.replies.lock().unwrap();
            if let Some(Reply::RenderFail(message)) = replies.front() {
                let message = message.clone();
                replies.pop_front();
                return Err(llm::Error::Decode(message));
            }
        }
        self.seen.lock().unwrap().push(request.clone());
        // No real wire format is needed: the harness never inspects the
        // rendered bytes, it only needs `open` to succeed so a frame exists.
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
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("more model calls than scripted upstream responses");
        match reply {
            Reply::Events(events) => {
                Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
            }
            Reply::Fail(error) => Box::pin(futures_util::stream::iter(vec![Err(error)])),
            Reply::MidStream(prefix, error) => Box::pin(futures_util::stream::iter(
                prefix
                    .into_iter()
                    .map(Ok)
                    .chain(std::iter::once(Err(error))),
            )),
            Reply::Timeout => {
                let addr = self.timeout_addr;
                let timeout = self.timeout;
                Box::pin(async_stream::stream! {
                    let error = reqwest::Client::builder()
                        .timeout(timeout)
                        .build()
                        .expect("a test client builds")
                        .get(format!("http://{addr}/"))
                        .send()
                        .await
                        .expect_err("the hanging listener never responds");
                    yield Err::<Event, _>(llm::Error::Transport(error));
                })
            }
            Reply::Refused => Box::pin(async_stream::stream! {
                let error = reqwest::Client::builder()
                    .timeout(Duration::from_secs(5))
                    .build()
                    .expect("a test client builds")
                    .get("http://127.0.0.1:1/")
                    .send()
                    .await
                    .expect_err("loopback port 1 must refuse the connection");
                yield Err::<Event, _>(llm::Error::Transport(error));
            }),
            Reply::RenderFail(_) => unreachable!("render consumed the RenderFail reply"),
        }
    }
}

/// A loopback listener that accepts connections and then never reads or
/// writes, so a client with a timeout produces a real
/// `llm::Error::Transport` whose `is_timeout()` is true. Independent of any
/// tokio runtime, so it outlives the isolate's and the test's.
fn spawn_hanging_listener() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback listener");
    let addr = listener.local_addr().expect("a bound address");
    std::thread::spawn(move || {
        // Hold every accepted socket open — dropping one would close it and
        // turn the timeout into a connection-reset.
        let mut held = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => held.push(stream),
                Err(_) => break,
            }
        }
    });
    addr
}

// ---------------------------------------------------------------------------
// Fixtures and helpers
// ---------------------------------------------------------------------------

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
/// closes cleanly and only folding the events fails — an internal invariant
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

/// Runs the shipped conversational harness to completion (success or failure)
/// against `client`, returning the transcript and the outcome.
fn run_conversational(client: Arc<dyn ModelClient>) -> (Vec<Value>, harness::RunOutcome) {
    let mut run = HarnessRun::start(
        harness::CONVERSATIONAL.to_owned(),
        input("hi"),
        grant(client),
        Seed::default(),
    );
    let events = support::drain_transcript(&mut run);
    let outcome = run
        .join()
        .expect("a run that threw still returns its frame log");
    (events, outcome)
}

fn retry_events(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|event| event["type"] == "retry")
        .collect()
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

/// The assistant text the harness actually committed, joined across blocks.
fn committed_text(outcome: &harness::RunOutcome) -> String {
    outcome
        .committed
        .messages
        .iter()
        .filter(|message| matches!(message.role, llm::Role::Assistant))
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            llm::ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// Upstream failures the conversational harness retries
// ---------------------------------------------------------------------------

#[test]
fn a_timeout_is_retried_and_the_conversation_still_answers() {
    let client = Arc::new(Upstream::new(vec![
        Reply::Timeout,
        Reply::Events(text_reply("recovered from the timeout")),
    ]));
    let (events, outcome) = run_conversational(client.clone());

    // A real timeout reads as a transient transport failure, so the shipped
    // retry loop caught it and tried again.
    let retries = retry_events(&events);
    assert_eq!(retries.len(), 1, "one retry after the first timeout");
    assert_eq!(retries[0]["attempt"], json!(1));
    assert_eq!(retries[0]["maxRetries"], json!(3));

    assert!(
        events.iter().any(|event| event["type"] == "block_delta"
            && event["delta"]["text"] == json!("recovered from the timeout")),
        "the retry's answer must reach the transcript"
    );
    assert!(
        committed_text(&outcome).contains("recovered from the timeout"),
        "and the conversation must commit it"
    );

    assert_eq!(client.requests_seen().len(), 2, "both attempts dialed out");
    assert_eq!(frame_starts(&outcome.frames), 2);
    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1, "only the first attempt's frame failed");
    assert_eq!(failed[0].kind, "transport");
    assert!(failed[0].transient);
}

#[test]
fn a_refused_dial_is_retried_like_any_other_transient_transport_failure() {
    let client = Arc::new(Upstream::new(vec![
        Reply::Refused,
        Reply::Events(text_reply("recovered from the refused dial")),
    ]));
    let (events, outcome) = run_conversational(client.clone());

    assert_eq!(retry_events(&events).len(), 1);
    assert!(
        committed_text(&outcome).contains("recovered from the refused dial"),
        "a connect failure is retryable, not terminal"
    );
    assert_eq!(client.requests_seen().len(), 2);
}

#[test]
fn a_bad_gateway_is_retried_and_the_conversation_still_answers() {
    let client = Arc::new(Upstream::new(vec![
        Reply::Fail(api_error(502, "api_error", "Bad Gateway", None)),
        Reply::Events(text_reply("recovered from the gateway")),
    ]));
    let (events, outcome) = run_conversational(client.clone());

    assert_eq!(retry_events(&events).len(), 1);
    assert!(
        committed_text(&outcome).contains("recovered from the gateway"),
        "a retried 502 must still finish the conversation"
    );

    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "api");
    assert!(failed[0].transient);
    assert_eq!(failed[0].status, Some(502));
}

#[test]
fn a_rate_limit_is_retried_and_its_status_reaches_the_frame_log() {
    let client = Arc::new(Upstream::new(vec![
        Reply::Fail(api_error(
            429,
            "rate_limit_error",
            "Rate limit reached for the model",
            Some("req_rl_1"),
        )),
        Reply::Events(text_reply("recovered from the rate limit")),
    ]));
    let (events, outcome) = run_conversational(client.clone());

    let retries = retry_events(&events);
    assert_eq!(retries.len(), 1);
    assert_eq!(
        retries[0]["attempt"],
        json!(1),
        "a 429 is transient, so the shipped loop backs off and retries"
    );

    // The status and request id survive the failing attempt into the frame
    // log, which is where Core — not the harness — records what happened.
    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "api");
    assert!(failed[0].transient);
    assert_eq!(failed[0].status, Some(429));
    assert_eq!(failed[0].request_id.as_deref(), Some("req_rl_1"));

    assert!(committed_text(&outcome).contains("recovered from the rate limit"));
}

#[test]
fn a_mid_stream_break_keeps_what_arrived_and_the_retry_finishes_the_turn() {
    let client = Arc::new(Upstream::new(vec![
        Reply::MidStream(
            streaming_text_prefix("partial"),
            api_error(
                503,
                "overloaded_error",
                "upstream closed the connection mid-stream",
                None,
            ),
        ),
        Reply::Events(text_reply("finished after the break")),
    ]));
    let (events, outcome) = run_conversational(client.clone());

    assert!(
        events
            .iter()
            .any(|event| event["type"] == "block_delta"
                && event["delta"]["text"] == json!("partial")),
        "text that arrived before the break must survive it in the transcript"
    );
    assert_eq!(
        retry_events(&events).len(),
        1,
        "a 503 mid-stream is transient and gets retried"
    );
    assert!(
        committed_text(&outcome).contains("finished after the break"),
        "only the completed call is committed, and it is the retry's"
    );

    assert_eq!(frame_starts(&outcome.frames), 2);
    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].status, Some(503));
}

#[test]
fn retries_exhausted_surface_the_last_failure_and_leave_the_lineage_untouched() {
    // MAX_RETRIES is 3, so the loop makes four attempts before giving up.
    // Every one fails transiently — the shipped backoff runs, and the run
    // still ends in error rather than pretending an answer arrived.
    let client = Arc::new(Upstream::new(vec![
        Reply::Fail(api_error(502, "api_error", "Bad Gateway", None)),
        Reply::Fail(api_error(502, "api_error", "Bad Gateway", None)),
        Reply::Fail(api_error(502, "api_error", "Bad Gateway", None)),
        Reply::Fail(api_error(502, "api_error", "Bad Gateway", None)),
    ]));
    let (events, outcome) = run_conversational(client.clone());

    assert_eq!(
        retry_events(&events).len(),
        3,
        "three retries between four attempts"
    );
    assert_eq!(
        client.requests_seen().len(),
        4,
        "the loop must stop at MAX_RETRIES, not spin"
    );
    assert_eq!(frame_starts(&outcome.frames), 4);
    assert_eq!(failed_frames(&outcome.frames).len(), 4);

    assert!(
        outcome.error.is_some(),
        "an exhausted retry budget ends the run in error"
    );
    assert!(
        outcome.committed.messages.is_empty(),
        "a failed run must not advance the lineage"
    );
    assert!(
        outcome.committed_frame.is_none(),
        "and there is no committed position to record"
    );
}

// ---------------------------------------------------------------------------
// Internal crashes the conversational harness must not retry
// ---------------------------------------------------------------------------

#[test]
fn a_render_failure_is_not_retried_and_ends_the_run() {
    let client = Arc::new(Upstream::render_failing("definitely not wire bytes"));
    let (events, outcome) = run_conversational(client.clone());

    assert!(
        retry_events(&events).is_empty(),
        "a non-transient failure must not be answered with another call"
    );
    assert_eq!(
        frame_starts(&outcome.frames),
        0,
        "a call refused at open must log no frame at all"
    );
    assert!(
        client.requests_seen().is_empty(),
        "nothing was rendered, so nothing could have been sent"
    );
    assert!(outcome.error.is_some(), "the crash must end the run");
    assert!(
        outcome.committed.messages.is_empty(),
        "a crashed run commits nothing"
    );
}

#[test]
fn a_truncated_tool_call_is_an_internal_crash_not_a_retryable_call() {
    let client = Arc::new(Upstream::new(vec![Reply::Events(truncated_tool_call())]));
    let (events, outcome) = run_conversational(client.clone());

    assert!(
        retry_events(&events).is_empty(),
        "malformed arguments will not repair themselves on retry"
    );
    assert_eq!(client.requests_seen().len(), 1);
    assert_eq!(frame_starts(&outcome.frames), 1);
    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "tool_input");
    assert!(!failed[0].transient);
    assert!(
        outcome.error.is_some(),
        "an unfoldable stream ends the run, it does not answer"
    );
}

#[test]
fn an_empty_stream_is_a_terminal_failure_the_harness_does_not_retry() {
    let client = Arc::new(Upstream::new(vec![Reply::Events(Vec::new())]));
    let (events, outcome) = run_conversational(client.clone());

    assert!(
        retry_events(&events).is_empty(),
        "an empty response is not transient, so it is not retried"
    );
    assert_eq!(frame_starts(&outcome.frames), 1);
    let failed = failed_frames(&outcome.frames);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, "empty_response");
    assert!(!failed[0].transient);
    assert!(outcome.error.is_some());
}
