//! End-to-end: a stop raised from outside a live run.
//!
//! The contract being pinned is `abstract.md`'s — a stop is not an unwind. It
//! reaches the loop as an ordinary failure of whatever outbound step was in
//! flight, carrying the `cancelled` kind, and the loop decides what to do
//! about it. Every test here therefore asserts on what the loop *saw*, not on
//! the run merely having ended.

mod support;

use std::sync::Arc;
use std::time::Duration;

use harness::mapping::LLMRequest;
use harness::{Context, Error, HarnessRun, Interrupter, Seed};
use llm::{
    BlockStart, Delta, Event, EventStream, ModelClient, RenderedRequest, RenderedSpan, Request,
    ToolDef, UsageDelta,
};
use serde_json::{Value, json};
use support::{Wire, forward, grant, history_and, input};

/// A model that starts answering and then never says another word — the
/// shape a stop actually has to interrupt. A script that simply ends would
/// let the run finish on its own and prove nothing.
struct Stalling;

impl ModelClient for Stalling {
    fn provider(&self) -> &str {
        "scripted"
    }

    fn render(&self, request: &Request) -> llm::Result<RenderedRequest> {
        Ok(RenderedRequest {
            body: Vec::new(),
            prefix: RenderedSpan {
                provider: "scripted".into(),
                model: request.model.clone(),
                reasoning: request
                    .reasoning_history
                    .unwrap_or(llm::ReasoningHistory::Full),
                regions: Default::default(),
            },
        })
    }

    fn send(&self, _rendered: RenderedRequest) -> EventStream<'_> {
        let opening = vec![
            Event::MessageStart {
                id: "msg_1".into(),
                model: "test-model".into(),
                usage: UsageDelta::default(),
            },
            Event::BlockStart {
                index: 0,
                block: BlockStart::Text,
            },
            Event::BlockDelta {
                index: 0,
                delta: Delta::Text {
                    content: "thinking about it".into(),
                },
            },
        ];
        Box::pin(async_stream::stream! {
            for event in opening {
                yield Ok(event);
            }
            std::future::pending::<()>().await;
        })
    }
}

/// Drains a run's transcript, raising the stop as soon as the run has
/// visibly started.
///
/// Tied to the first transcript event rather than a sleep: the point is that
/// the stop lands mid-answer, and a timer would sometimes land before the
/// loop started and test the pre-start path by accident.
async fn drain_interrupting_at_first_event(
    mut run: HarnessRun,
    interrupter: Interrupter,
    timeout: Duration,
) -> (Vec<Value>, Result<harness::RunOutcome, harness::RunError>) {
    tokio::time::timeout(timeout, async {
        let mut events: Vec<Value> = Vec::new();
        while let Some(event) = run.transcript.recv().await {
            if events.is_empty() {
                interrupter.raise();
            }
            events.push(event);
        }
        (events, run.join().await)
    })
    .await
    .unwrap_or_else(|_| {
        panic!("the run did not stop within {timeout:?} — a stop that hangs is worse than none")
    })
}

fn caught(events: &[Value]) -> Option<&Value> {
    events.iter().find(|event| event["type"] == "caught")
}

/// Streams one call into the transcript and reports a failure instead of
/// ending on it.
async fn catching(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
    let attempt = async {
        let call = ctx.stream(LLMRequest::new(history_and(&ctx, &input)))?;
        forward(&ctx, &call).await
    };
    if let Err(error) = attempt.await {
        ctx.emit(json!({ "type": "caught", "kind": error.kind() }));
    }
    Ok(())
}

#[tokio::test]
async fn a_stop_mid_answer_reaches_the_loop_as_a_cancelled_failure() {
    let (interrupter, interrupt) = harness::interrupt();
    let mut grant = grant(Arc::new(Stalling));
    grant.interrupt = Some(interrupt);

    let run = HarnessRun::start_with(catching, input("hi"), grant, Seed::default());
    let (events, outcome) =
        drain_interrupting_at_first_event(run, interrupter, Duration::from_secs(10)).await;

    // Ended by its own error handling, not by being terminated: the signal
    // is the mechanism, and an outcome with no error is what says the
    // backstop was never needed.
    let outcome = outcome.expect("a run always leaves an outcome");
    assert!(
        outcome.error.is_none(),
        "a loop that handles the stop must still finish cleanly"
    );

    let caught = caught(&events).expect("the loop must see the stop as a failure it can handle");
    assert_eq!(caught["kind"], "cancelled");

    // What did stream before the stop is still the user's conversation, and
    // the frame stopped failed because the call never produced a completion.
    assert!(
        events.iter().any(|event| event["type"] == "block_delta"),
        "text that arrived before the stop must survive it"
    );
    use harness::frame::{CoreEvent, Outcome};
    assert!(
        outcome.frames.iter().any(|frame| matches!(
            frame,
            CoreEvent::FrameStop {
                outcome: Outcome::Failed { error },
                ..
            } if error.kind == "cancelled"
        )),
        "the frame log must record why the call ended"
    );
}

/// Tries the same call twice, reporting each failure.
async fn retrying(ctx: Context, input: Vec<Wire>) -> Result<(), Error> {
    let messages = history_and(&ctx, &input);
    for attempt in 0..2 {
        let tried = async {
            let call = ctx.stream(LLMRequest::new(messages.clone()))?;
            forward(&ctx, &call).await
        };
        if let Err(error) = tried.await {
            ctx.emit(json!({ "type": "caught", "attempt": attempt, "kind": error.kind() }));
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_stopped_run_cannot_open_another_call() {
    let (interrupter, interrupt) = harness::interrupt();
    let mut grant = grant(Arc::new(Stalling));
    grant.interrupt = Some(interrupt);

    let run = HarnessRun::start_with(retrying, input("hi"), grant, Seed::default());
    let (events, outcome) =
        drain_interrupting_at_first_event(run, interrupter, Duration::from_secs(10)).await;
    let outcome = outcome.expect("the retrying loop ends on its own once both attempts fail");

    let caught: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "caught")
        .collect();
    assert_eq!(caught.len(), 2, "both attempts must fail");
    assert_eq!(caught[1]["kind"], "cancelled");

    // Handling the stop is allowed; answering it by dialling the provider
    // again is not. The second attempt was refused when it was opened, before
    // a frame existed for it.
    use harness::frame::CoreEvent;
    let opened = outcome
        .frames
        .iter()
        .filter(|frame| matches!(frame, CoreEvent::FrameStart { .. }))
        .count();
    assert_eq!(opened, 1, "no second call may reach the provider");
}

async fn tool_using(ctx: Context, _input: Vec<Wire>) -> Result<(), Error> {
    ctx.emit(json!({ "type": "invoking" }));
    match ctx.invoke_tool("wait", json!({})).await {
        Ok(_) => ctx.emit(json!({ "type": "returned" })),
        Err(error) => ctx.emit(json!({ "type": "caught", "kind": error.kind() })),
    }
    Ok(())
}

#[tokio::test]
async fn a_stop_during_a_tool_call_abandons_it() {
    let (interrupter, interrupt) = harness::interrupt();
    let mut grant = grant(Arc::new(Stalling));
    grant.interrupt = Some(interrupt);
    grant.tools = vec![ToolDef {
        name: "wait".into(),
        description: "never answers".into(),
        input_schema: json!({ "type": "object" }),
    }];
    // A tool that never returns: without a stop, this run does not end.
    grant.tool_invoker = Some(Arc::new(|_name, _input| Box::pin(std::future::pending())));

    let run = HarnessRun::start_with(tool_using, Vec::new(), grant, Seed::default());
    let (events, outcome) =
        drain_interrupting_at_first_event(run, interrupter, Duration::from_secs(10)).await;
    outcome.expect("abandoning the tool call must leave the run able to finish");

    let caught = caught(&events).expect("the stop must surface at the awaited invocation");
    assert_eq!(caught["kind"], "cancelled");
    assert!(
        !events.iter().any(|event| event["type"] == "returned"),
        "an abandoned tool call must not appear to have returned"
    );
}

#[tokio::test]
async fn a_run_stopped_before_it_starts_never_reaches_the_provider() {
    let (interrupter, interrupt) = harness::interrupt();
    interrupter.raise();

    let client = Arc::new(support::Scripted::new(support::text_reply("hello back")));
    let mut grant = grant(client.clone());
    grant.interrupt = Some(interrupt);

    // The run itself still starts — `crates/api` short-circuits earlier, but
    // the loop has to behave if it doesn't, and the refusal lands where the
    // call is opened, when nothing has been sent yet.
    let run = HarnessRun::start_with(catching, input("hi"), grant, Seed::default());
    let (events, outcome) =
        drain_interrupting_at_first_event(run, interrupter.clone(), Duration::from_secs(10)).await;
    outcome.expect("a refusal at open is an ordinary failure, not a broken run");

    assert_eq!(
        caught(&events).map(|event| event["kind"].clone()),
        Some(json!("cancelled"))
    );
    assert!(
        client.requests_seen().is_empty(),
        "a run stopped before it opened a call must render nothing"
    );
}

#[tokio::test]
async fn terminating_stops_a_loop_that_ignores_the_signal() {
    // The backstop: this loop never takes another outbound step after its
    // first event, so no signal can reach it, and only terminating the run
    // ends it.
    async fn waiting(ctx: Context, _input: Vec<Wire>) -> Result<(), Error> {
        ctx.emit(json!({ "type": "waiting" }));
        std::future::pending().await
    }

    let mut run = HarnessRun::start_with(
        waiting,
        Vec::new(),
        grant(Arc::new(Stalling)),
        Seed::default(),
    );
    let terminator = run.terminator();

    assert_eq!(
        run.transcript
            .recv()
            .await
            .map(|event| event["type"].clone()),
        Some(json!("waiting"))
    );
    terminator.terminate();

    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        while run.transcript.recv().await.is_some() {}
        run.join().await
    })
    .await
    .expect("terminating must end the run")
    .expect("a terminated run still returns its frame log");
    assert!(
        matches!(outcome.error, Some(harness::RunError::Terminated)),
        "a terminated run must retain its terminal error alongside the frame log"
    );
}
