//! Shared test fixtures: a scripted [`llm::ModelClient`] and small helpers
//! for building a [`Grant`] and draining a [`HarnessRun`].

#![allow(dead_code, reason = "not every test file uses every helper")]

use std::sync::{Arc, Mutex};

use harness::state::Baseline;
use harness::{Grant, HarnessRun, RunOutcome, Seed};
use llm::{
    BlockStart, ContentBlock, Delta, Event, EventStream, Message, ModelClient, RenderedRequest,
    RenderedSpan, Request, Role, StopReason, Turn, UsageDelta,
};

/// A model that replays one script per call, in order, and records every
/// request it rendered. Cycles the last script forever once exhausted.
pub struct Scripted {
    scripts: Mutex<Vec<Vec<Event>>>,
    seen: Mutex<Vec<Request>>,
}

impl Scripted {
    pub fn new(events: Vec<Event>) -> Self {
        Self::sequence(vec![events])
    }

    pub fn sequence(scripts: Vec<Vec<Event>>) -> Self {
        Self {
            scripts: Mutex::new(scripts),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn requests_seen(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

impl ModelClient for Scripted {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        self.seen.lock().unwrap().push(request.clone());

        // No real wire format: a minimal region splice good enough to prove
        // ids round-trip through the ops layer, without depending on any
        // real provider's shape. The harness crate never constructs
        // `Turn::Span` any more — that arm is only reachable from a bug in
        // `crates/llm` itself, which `span.rs`'s own tests already cover.
        let mut messages_bytes = Vec::new();
        for turn in request.messages.iter() {
            match turn {
                Turn::Span(span) => {
                    if let Some(bytes) = span.regions.get("messages") {
                        messages_bytes.extend_from_slice(bytes);
                    }
                }
                Turn::Value(message) => {
                    if !messages_bytes.is_empty() {
                        messages_bytes.push(b',');
                    }
                    messages_bytes.extend_from_slice(&serde_json::to_vec(message).unwrap());
                }
            }
        }
        let prefix = RenderedSpan {
            provider: "scripted".into(),
            model: request.model.clone(),
            reasoning: reasoning(request),
            regions: [("messages".to_string(), messages_bytes)]
                .into_iter()
                .collect(),
        };
        Ok(RenderedRequest {
            body: Vec::new(),
            prefix,
        })
    }

    fn send(&self, _rendered: RenderedRequest) -> EventStream<'_> {
        let mut scripts = self.scripts.lock().unwrap();
        let events = if scripts.len() > 1 {
            scripts.remove(0)
        } else {
            scripts.first().cloned().unwrap_or_default()
        };
        Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
    }
}

/// A model that renders through the real Anthropic wire module and scripts
/// only `send`.
///
/// `Scripted` fabricates its own messages region, which is fine for proving
/// spans and ids route correctly through the ops layer but cannot support an
/// assertion that a later request's rendered bytes are byte-identical to what
/// an earlier one *actually sent* — that needs a real renderer. `Recording`
/// exists for exactly that class of test: it never dials out (`send` is
/// scripted, `render` never touches the network), but the bytes it produces
/// are the provider's, not a fixture's.
pub struct Recording {
    scripts: Mutex<Vec<Vec<Event>>>,
    requests: Mutex<Vec<Request>>,
    rendered: Mutex<Vec<RenderedRequest>>,
}

impl Recording {
    pub fn new(events: Vec<Event>) -> Self {
        Self::sequence(vec![events])
    }

    pub fn sequence(scripts: Vec<Vec<Event>>) -> Self {
        Self {
            scripts: Mutex::new(scripts),
            requests: Mutex::new(Vec::new()),
            rendered: Mutex::new(Vec::new()),
        }
    }

    pub fn requests_seen(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// Every `RenderedRequest` that crossed the ops boundary, in order.
    pub fn rendered(&self) -> Vec<RenderedRequest> {
        self.rendered.lock().unwrap().clone()
    }
}

impl ModelClient for Recording {
    fn provider(&self) -> &str {
        "anthropic"
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        self.requests.lock().unwrap().push(request.clone());
        let rendered = llm::anthropic::wire::render(request)?;
        self.rendered.lock().unwrap().push(rendered.clone());
        Ok(rendered)
    }

    fn send(&self, _rendered: RenderedRequest) -> EventStream<'_> {
        let mut scripts = self.scripts.lock().unwrap();
        let events = if scripts.len() > 1 {
            scripts.remove(0)
        } else {
            scripts.first().cloned().unwrap_or_default()
        };
        Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
    }
}

/// A model whose stream emits a few events and then a terminal error — for
/// exercising the two failure paths `advance()` handles: a stream event
/// that's itself an `Err`, and (via a harness's own retry) an already-failed
/// slot. `render` is a stub that never touches the network.
pub struct Failing;

impl ModelClient for Failing {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        Ok(RenderedRequest {
            body: Vec::new(),
            prefix: RenderedSpan {
                provider: "scripted".into(),
                model: request.model.clone(),
                reasoning: reasoning(request),
                regions: Default::default(),
            },
        })
    }

    fn send(&self, _rendered: RenderedRequest) -> EventStream<'_> {
        let events: Vec<llm::Result<Event>> = vec![
            Ok(Event::MessageStart {
                id: "msg_fail".into(),
                model: "test-model".into(),
                usage: UsageDelta {
                    input_tokens: Some(5),
                    ..UsageDelta::default()
                },
            }),
            Ok(Event::BlockStart {
                index: 0,
                block: BlockStart::Text,
            }),
            Ok(Event::BlockDelta {
                index: 0,
                delta: Delta::Text {
                    content: "partial".into(),
                },
            }),
            Err(llm::Error::EmptyResponse),
        ];
        Box::pin(futures_util::stream::iter(events))
    }
}

pub fn text_reply(text: &str) -> Vec<Event> {
    vec![
        Event::MessageStart {
            id: "msg_1".into(),
            model: "test-model".into(),
            usage: UsageDelta {
                input_tokens: Some(10),
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
        Event::BlockStop { index: 0 },
        Event::MessageDelta {
            stop_reason: Some(StopReason::EndTurn),
            stop_details: None,
            usage: UsageDelta {
                output_tokens: Some(4),
                ..UsageDelta::default()
            },
        },
        Event::MessageStop,
    ]
}

pub fn tool_call_reply(id: &str, name: &str, args_json: &str) -> Vec<Event> {
    vec![
        Event::MessageStart {
            id: "msg_2".into(),
            model: "test-model".into(),
            usage: UsageDelta::default(),
        },
        Event::BlockStart {
            index: 0,
            block: BlockStart::ToolUse {
                id: id.into(),
                name: name.into(),
            },
        },
        Event::BlockDelta {
            index: 0,
            delta: Delta::ToolInputJson {
                content: args_json.into(),
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

/// What a fake render records as the span's scope. No wire here has an
/// opinion, so an unset request takes the same default the Anthropic wire
/// does — the point is only that a span is scoped to the policy it was made
/// under.
fn reasoning(request: &Request) -> llm::ReasoningHistory {
    request
        .reasoning_history
        .unwrap_or(llm::ReasoningHistory::Full)
}

pub fn grant(client: Arc<dyn ModelClient>) -> Grant {
    Grant {
        client,
        model: "test-model".into(),
        reasoning_history: None,
        // No caller opinion by default — the committed lineage's own
        // thinking/effort carry across runs. `grant_reasoning` is the opposite
        // case.
        reasoning: None,
        advanced_options: llm::AdvancedOptions::default(),
        tools: Vec::new(),
        tool_invoker: None,
        commit_granted: true,
        functions: harness::FunctionRegistry::new(),
        // Tests that exercise a stop build their own pair and set it here.
        interrupt: None,
    }
}

/// [`grant`], with the caller's reasoning selection resolved — what
/// `crates/api` builds once a model declares a thinking knob.
pub fn grant_reasoning(client: Arc<dyn ModelClient>, reasoning: harness::Reasoning) -> Grant {
    Grant {
        reasoning: Some(reasoning),
        ..grant(client)
    }
}

/// What a prior run committed, for tests exercising the cross-run case
/// directly — the workflow-side spine that would normally produce this
/// (`crates/api`) doesn't exist yet.
pub fn seed(provider: &str, model: &str, messages: Vec<Message>) -> Seed {
    Seed {
        provider: provider.into(),
        model: model.into(),
        messages,
        options: Baseline::default(),
    }
}

/// One turn's input. A `Vec` because that is what a run takes — a turn is
/// usually one message and does not have to be.
pub fn input(text: &str) -> Vec<Message> {
    vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: text.into() }],
    }]
}

/// Drains a run's transcript into a `Vec`, blocking the current (sync test)
/// thread on a throwaway tokio runtime — tests don't need real concurrency,
/// just a receiver to poll.
pub fn drain_transcript(run: &mut HarnessRun) -> Vec<serde_json::Value> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut events = Vec::new();
        while let Some(event) = run.transcript.recv().await {
            events.push(event);
        }
        events
    })
}

/// Joins a run that is expected to finish on its own terms, failing the test
/// with `why` if it did not.
///
/// `join` returns `Ok` for a run whose module threw — the error rides along in
/// [`RunOutcome::error`] so a failed run's frames survive for diagnosis. That
/// makes a bare `join().expect(..)` assert almost nothing, so every test that
/// means "this run finished cleanly" says it through here instead.
pub fn finished(run: HarnessRun, why: &str) -> RunOutcome {
    let outcome = run.join().unwrap_or_else(|error| {
        panic!("{why} — the run produced no outcome at all: {error}");
    });
    if let Some(error) = &outcome.error {
        panic!("{why} — the harness ended in error: {error}");
    }
    outcome
}

/// Like draining and joining a run in sequence, except a run that never
/// finishes fails this test with a clear message instead of hanging the
/// whole suite. For a test whose entire point is proving a specific op
/// sequence *doesn't* deadlock the isolate — an assertion that regresses to
/// an actual hang is the worst possible failure mode for it to have.
pub fn drain_and_join_with_timeout(
    mut run: HarnessRun,
    timeout: std::time::Duration,
) -> (Vec<serde_json::Value>, RunOutcome) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let events = drain_transcript(&mut run);
        let outcome = run.join();
        let _ = tx.send((events, outcome));
    });
    match rx.recv_timeout(timeout) {
        Ok((events, Ok(outcome))) => {
            // `join` reports a failed *module* through the outcome, not through
            // `Err` — without this, a run that threw would reach the caller's
            // assertions looking exactly like one that finished.
            if let Some(error) = &outcome.error {
                panic!("run failed: {error}");
            }
            (events, outcome)
        }
        Ok((_, Err(error))) => panic!("run failed before it could produce an outcome: {error}"),
        Err(_) => panic!(
            "run did not finish within {timeout:?} — this looks like a hang, not a slow test"
        ),
    }
}
