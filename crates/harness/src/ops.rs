//! The op layer: everything `faber:context.js` calls into.
//!
//! Every op reaches shared state through `Rc<RefCell<HarnessState>>`, put
//! into `OpState` once at startup (`runtime.rs`). Async ops take
//! `Rc<RefCell<OpState>>` and are careful never to hold that borrow, or our
//! own `RefCell` borrow, across an `.await` — the pattern throughout is:
//! borrow, extract what's needed as owned values, drop the borrow, await,
//! re-borrow to store the result.

use std::cell::RefCell;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;

use deno_core::{OpState, op2};
use futures_util::{Stream, StreamExt};
use llm::{Accumulator, ModelClient, RenderedRequest};
use serde::Serialize;

use crate::error::OpError;
use crate::frame::{CoreEvent, FrameDetail, Outcome};
use crate::mapping::{
    CommitOptions, Completion, HarnessErrorInfo, LLMEvent, LLMRequest, Message, RequestOptions,
    ToolDef, ToolResult,
};
use crate::state::{Baseline, Committed, HarnessState, Sent, SentTurn, StreamSlot};
use crate::validate::Refusal;

fn shared(state: &OpState) -> Rc<RefCell<HarnessState>> {
    state.borrow::<Rc<RefCell<HarnessState>>>().clone()
}

async fn shared_async(state: &Rc<RefCell<OpState>>) -> Rc<RefCell<HarnessState>> {
    let op_state = state.borrow();
    shared(&op_state)
}

/// Whether a stop has been asked for from outside this run
/// (`crate::interrupt`). A run that was granted no interrupt is one nobody
/// can stop, and reads as never stopped.
fn stopped(harness: &HarnessState) -> bool {
    harness
        .grant
        .interrupt
        .as_ref()
        .is_some_and(crate::interrupt::Interrupt::raised)
}

// ---------------------------------------------------------------------------
// Capabilities and tools
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Capabilities {
    pub commit: bool,
}

#[op2]
#[serde]
pub fn op_capabilities(state: &mut OpState) -> Capabilities {
    let harness = shared(state);
    let harness = harness.borrow();
    Capabilities {
        commit: harness.grant.commit_granted,
    }
}

#[op2]
#[serde]
pub fn op_tools_available(state: &mut OpState) -> Vec<ToolDef> {
    let harness = shared(state);
    let harness = harness.borrow();
    harness.grant.tools.iter().map(ToolDef::from).collect()
}

#[op2]
#[serde]
pub fn op_functions_available(state: &mut OpState) -> Vec<String> {
    let harness = shared(state);
    harness.borrow().grant.functions.names()
}

#[op2]
#[serde]
pub async fn op_function_invoke(
    state: Rc<RefCell<OpState>>,
    #[string] name: String,
    #[serde] input: serde_json::Value,
) -> Result<serde_json::Value, OpError> {
    let harness = shared_async(&state).await;
    let function = harness
        .borrow()
        .grant
        .functions
        .get(&name)
        .ok_or_else(|| OpError::UnknownFunction { name: name.clone() })?;

    // Logged here rather than left to the harness: a harness may treat a
    // failure as best effort and catch it, and the operator still needs to
    // know it happened.
    function(input).await.map_err(|message| {
        tracing::warn!(function = %name, error = %message, "granted function failed");
        OpError::FunctionFailed { name, message }
    })
}

#[op2]
#[serde]
pub async fn op_tool_invoke(
    state: Rc<RefCell<OpState>>,
    #[string] name: String,
    #[serde] input: serde_json::Value,
) -> Result<ToolResult, OpError> {
    let harness = shared_async(&state).await;

    let (invocation, interrupt) = {
        let harness = harness.borrow();
        let Some(invoker) = harness.grant.tool_invoker.as_ref() else {
            return Err(OpError::UnknownTool { name });
        };
        // Checked before dispatch as well as during it: a stop that landed
        // while the harness was between calls must not be answered by
        // starting fresh work outside the isolate.
        if stopped(&harness) {
            return Err(HarnessErrorInfo::cancelled().into());
        }
        (
            (invoker)(name.clone(), input),
            harness.grant.interrupt.clone(),
        )
    };

    let dispatched = match &interrupt {
        Some(signal) => tokio::select! {
            biased;
            // Dropping `invocation` here is what abandons the call. What the
            // tool already did on the other side is not undone — an
            // interrupted `exec` may well have written a file — so this is a
            // stop, not a rollback, and the environment layer is where any
            // stronger promise would have to live.
            () = signal.raised_at() => return Err(HarnessErrorInfo::cancelled().into()),
            dispatched = invocation => dispatched,
        },
        None => invocation.await,
    };

    dispatched
        .map(|result| ToolResult {
            content: result.content,
            is_error: result.is_error,
        })
        .map_err(|message| OpError::ToolDispatchFailed { name, message })
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

/// Exact, not reconstructed (§3.1) — `committed_messages` is the ground
/// truth Core holds, so this is a clone with ids attached, not a re-parse of
/// rendered bytes. The lossy reconstruction path this replaced
/// (`parse_region`/`reconstruct_message`/`reconstruct_content`) is gone: it
/// existed only because bytes, not turns, used to be canonical.
#[op2]
#[serde]
pub fn op_history_read(state: &mut OpState) -> Vec<Message> {
    let harness = shared(state);
    let harness = harness.borrow();
    harness
        .committed_messages
        .iter()
        .map(|committed| Message::committed(&committed.id, &committed.message))
        .collect()
}

/// The committed lineage's own request options (`proposal.md` §4) —
/// `Context.committedRequest()`'s answer, and the exact baseline
/// `op_llm_stream_open`'s merge fills unset fields from. Both read the same
/// stored `Baseline`, so they cannot independently drift.
#[op2]
#[serde]
pub fn op_committed_request(state: &mut OpState) -> RequestOptions {
    let harness = shared(state);
    let harness = harness.borrow();
    RequestOptions::from(&harness.baseline)
}

// ---------------------------------------------------------------------------
// Model calls
// ---------------------------------------------------------------------------

/// Merges the request against the committed baseline, validates it (§7),
/// renders, and opens a stream slot — but does not send. `types.d.ts`: "the
/// request is not sent until the stream is polled." Rendering (and
/// therefore validating) here rather than at first `next` means a refusal
/// surfaces synchronously at `open`, and the frame log records ground truth
/// (J1) the moment the call is opened, not after the network round trip
/// starts.
#[op2]
pub fn op_llm_stream_open(
    state: &mut OpState,
    #[serde] request: LLMRequest,
) -> Result<u32, OpError> {
    let harness = shared(state);
    let mut harness = harness.borrow_mut();

    // Refused before anything is rendered or logged. A harness is free to
    // catch a `cancelled` failure and keep going — that is what makes it an
    // ordinary failure — but "keep going" must not extend to opening new
    // calls against the provider after the user asked for a stop.
    if stopped(&harness) {
        return Err(HarnessErrorInfo::cancelled().into());
    }

    let baseline = harness.baseline.clone();

    // Every field but `messages` defaults to the committed baseline
    // (`proposal.md` §4) — an explicit override on the request always wins.
    let tools: Vec<llm::ToolDef> = match request.tools.clone() {
        Some(tools) => tools.into_iter().map(llm::ToolDef::from).collect(),
        None => baseline.tools.clone(),
    };
    let max_tokens = request.max_tokens.or(baseline.max_tokens);
    let tool_choice = request
        .tool_choice
        .clone()
        .map(llm::ToolChoice::from)
        .or_else(|| baseline.tool_choice.clone());
    let thinking = request
        .thinking
        .map(llm::Thinking::from)
        .or(baseline.thinking);
    let effort = request.effort.map(llm::Effort::from).or(baseline.effort);
    let sampling = request
        .sampling
        .map(llm::Sampling::from)
        .or(baseline.sampling);
    let stop_sequences = request
        .stop_sequences
        .clone()
        .or_else(|| baseline.stop_sequences.clone());
    let extra = request.extra.clone().or_else(|| baseline.extra.clone());

    let sent_turns = crate::validate::check(
        &request.messages,
        &harness.committed_messages,
        &tools,
        harness.grant.client.provider(),
        &harness.grant.model,
        &harness.scaffold,
    )
    .map_err(|refusal| OpError::from(&refusal))?;

    let mut llm_messages = Vec::with_capacity(sent_turns.len());
    for turn in &sent_turns {
        let message = match turn {
            SentTurn::Ref { id } => harness
                .resolve(id)
                .expect("validate::check only ever returns ids it resolved against this lineage")
                .1
                .clone(),
            SentTurn::Value(message) => message.clone(),
        };
        llm_messages.push(llm::Turn::Value(message));
    }

    let mut llm_request = llm::Request::new(harness.grant.model.clone(), vec![]);
    llm_request.messages = llm_messages;
    llm_request.max_tokens = max_tokens.unwrap_or(llm::DEFAULT_MAX_TOKENS);
    llm_request.tools = tools.clone();
    llm_request.tool_choice = tool_choice.clone();
    llm_request.thinking = thinking;
    llm_request.effort = effort;
    llm_request.reasoning_history = harness.grant.reasoning_history;
    llm_request.sampling = sampling.unwrap_or_default();
    llm_request.stop_sequences = stop_sequences.clone().unwrap_or_default();
    // The grant's advanced options are a floor, not a baseline field: they
    // must apply to every call for this model, even the first one where a
    // workflow's own `extra` overrides an unrelated key entirely (`or_else`
    // above, not a merge).
    llm_request.extra = {
        let mut merged = harness.grant.advanced_options.as_extra();
        merged.extend(extra.clone().unwrap_or_default());
        merged
    };

    let rendered = harness
        .grant
        .client
        .render(&llm_request)
        .map_err(|error| OpError::from(&error))?;

    let frame = harness.frame_counter.next_child();
    let model = harness.grant.model.clone();
    let _ = harness.frames_tx.send(CoreEvent::FrameStart {
        frame: frame.clone(),
        parent: Some(crate::frame::FrameCounter::ROOT.to_string()),
        detail: FrameDetail::Model { model },
    });
    // Immediately after the frame opens and before the slot exists, so the
    // bytes are logged whether or not this call is ever polled (J1).
    let _ = harness.frames_tx.send(CoreEvent::ModelRequest {
        frame: frame.clone(),
        body: rendered.body.clone(),
    });

    let sent = Sent {
        turns: sent_turns,
        options: Baseline {
            max_tokens,
            tools,
            tool_choice,
            thinking,
            effort,
            sampling,
            stop_sequences,
            extra,
        },
        frame,
    };

    let rid = harness.insert_stream(StreamSlot::Pending { rendered, sent });
    Ok(rid)
}

fn owned_stream(
    client: Arc<dyn ModelClient>,
    rendered: RenderedRequest,
) -> Pin<Box<dyn Stream<Item = llm::Result<llm::Event>>>> {
    Box::pin(async_stream::stream! {
        let stream = client.send(rendered);
        futures_util::pin_mut!(stream);
        while let Some(item) = stream.next().await {
            yield item;
        }
    })
}

/// Advances stream `rid` by exactly one event, updating its slot. Shared by
/// `op_llm_stream_next` (which reports each event to the harness) and
/// `op_llm_stream_completion`/`op_commit` (which drain silently if asked
/// before the harness finished iterating).
async fn advance(
    harness: &Rc<RefCell<HarnessState>>,
    rid: u32,
) -> Result<Option<LLMEvent>, OpError> {
    let (mut inner, mut accumulator, sent) = {
        let mut state = harness.borrow_mut();
        let slot = state
            .streams
            .remove(&rid)
            .ok_or(OpError::UnknownStream { rid })?;
        match slot {
            // Another `advance()` for this rid is between removing its slot
            // and re-inserting it (i.e. mid-`.await`). Put the marker back
            // and reject clearly, rather than letting this call also see
            // "missing" and misreport `UnknownStream` on a live stream.
            StreamSlot::Polling => {
                state.streams.insert(rid, StreamSlot::Polling);
                return Err(OpError::StreamBusy { rid });
            }
            StreamSlot::Pending { rendered, sent } => {
                let client = state.grant.client.clone();
                state.streams.insert(rid, StreamSlot::Polling);
                (owned_stream(client, rendered), Accumulator::new(), sent)
            }
            StreamSlot::Active {
                inner,
                accumulator,
                sent,
            } => {
                state.streams.insert(rid, StreamSlot::Polling);
                (inner, accumulator, sent)
            }
            StreamSlot::Done { sent, completion } => {
                state
                    .streams
                    .insert(rid, StreamSlot::Done { sent, completion });
                return Ok(None);
            }
            // Re-polling an already-failed stream returns its *original*
            // failure, not a misleading "unknown stream" — `Call.completion`
            // on a call that broke mid-iteration must see the real reason.
            StreamSlot::Failed {
                sent,
                partial,
                error,
            } => {
                let info = error.clone();
                state.streams.insert(
                    rid,
                    StreamSlot::Failed {
                        sent,
                        partial,
                        error,
                    },
                );
                return Err(info.into());
            }
        }
    };

    // Cloned out of `sent` because `sent` moves into whichever slot this
    // call lands in, while the frame is still needed to tag the log events
    // emitted alongside it.
    let frame = sent.frame.clone();
    let interrupt = harness.borrow().grant.interrupt.clone();

    // Observed at the await the run spends nearly all its time in, not only
    // between events: a provider mid-answer would otherwise keep the run open
    // for as long as it kept talking. A `Pending` slot that is stopped here
    // was never polled, so nothing was sent at all.
    let next = match &interrupt {
        Some(signal) => tokio::select! {
            biased;
            () = signal.raised_at() => None,
            event = inner.next() => Some(event),
        },
        None => Some(inner.next().await),
    };

    let Some(next) = next else {
        // Terminal, and shaped exactly like a stream that broke: whatever the
        // accumulator folded is kept, so `commit(call, { partial: true })`
        // (§6.3) still has something to adopt, and the frame stops failed
        // because this call never produced a completion. Dropping `inner` is
        // what closes the provider connection.
        let info = HarnessErrorInfo::cancelled();
        let partial = accumulator.finish().ok();
        let mut state = harness.borrow_mut();
        if let Some(completion) = &partial {
            let _ = state.frames_tx.send(CoreEvent::ModelUsage {
                frame: frame.clone(),
                usage: completion.usage,
            });
        }
        let _ = state.frames_tx.send(CoreEvent::FrameStop {
            frame,
            outcome: Outcome::Failed {
                error: info.clone(),
            },
        });
        state.streams.insert(
            rid,
            StreamSlot::Failed {
                sent,
                partial,
                error: info.clone(),
            },
        );
        return Err(info.into());
    };

    match next {
        Some(Ok(event)) => {
            let mapped = LLMEvent::from(event.clone());
            accumulator.push(&event);
            let mut state = harness.borrow_mut();
            let _ = state.frames_tx.send(CoreEvent::ModelEvent {
                frame: frame.clone(),
                event,
            });
            state.streams.insert(
                rid,
                StreamSlot::Active {
                    inner,
                    accumulator,
                    sent,
                },
            );
            Ok(Some(mapped))
        }
        Some(Err(error)) => {
            let info = HarnessErrorInfo::from(&error);
            log_model_failure(&harness.borrow(), &frame, &info);
            // Whatever the accumulator folded before the stream broke is
            // recovered here, best-effort, so `commit(call, { partial: true
            // })` (§6.3) has something to adopt.
            let partial = accumulator.finish().ok();
            let mut state = harness.borrow_mut();
            if let Some(completion) = &partial {
                let _ = state.frames_tx.send(CoreEvent::ModelUsage {
                    frame: frame.clone(),
                    usage: completion.usage,
                });
            }
            let _ = state.frames_tx.send(CoreEvent::FrameStop {
                frame: frame.clone(),
                outcome: Outcome::Failed {
                    error: info.clone(),
                },
            });
            state.streams.insert(
                rid,
                StreamSlot::Failed {
                    sent,
                    partial,
                    error: info.clone(),
                },
            );
            Err(info.into())
        }
        None => {
            let mut state = harness.borrow_mut();
            match accumulator.finish() {
                Ok(completion) => {
                    let _ = state.frames_tx.send(CoreEvent::ModelUsage {
                        frame: frame.clone(),
                        usage: completion.usage,
                    });
                    let _ = state.frames_tx.send(CoreEvent::FrameStop {
                        frame,
                        outcome: Outcome::Ok,
                    });
                    state
                        .streams
                        .insert(rid, StreamSlot::Done { sent, completion });
                    Ok(None)
                }
                Err(error) => {
                    // `Accumulator::finish` consumed itself failing (e.g.
                    // truncated tool-call JSON) — there is no completion left
                    // to recover, so `partial` is genuinely `None` here, not
                    // just unset.
                    let info = HarnessErrorInfo::from(&error);
                    log_model_failure(&state, &frame, &info);
                    let _ = state.frames_tx.send(CoreEvent::FrameStop {
                        frame,
                        outcome: Outcome::Failed {
                            error: info.clone(),
                        },
                    });
                    state.streams.insert(
                        rid,
                        StreamSlot::Failed {
                            sent,
                            partial: None,
                            error: info.clone(),
                        },
                    );
                    Err(info.into())
                }
            }
        }
    }
}

#[op2]
#[serde]
pub async fn op_llm_stream_next(
    state: Rc<RefCell<OpState>>,
    rid: u32,
) -> Result<Option<LLMEvent>, OpError> {
    let harness = shared_async(&state).await;
    advance(&harness, rid).await
}

/// Drains to a terminal state and returns the completion — `Call.completion`.
/// A failed call rejects with the failure that actually happened, whether
/// this call is what triggered it or it had already happened by the time
/// this was awaited.
#[op2]
#[serde]
pub async fn op_llm_stream_completion(
    state: Rc<RefCell<OpState>>,
    rid: u32,
) -> Result<Completion, OpError> {
    let harness = shared_async(&state).await;

    loop {
        let is_done = {
            let harness = harness.borrow();
            matches!(harness.streams.get(&rid), Some(StreamSlot::Done { .. }))
        };
        if is_done {
            break;
        }
        advance(&harness, rid).await?;
    }

    let harness = harness.borrow();
    match harness.streams.get(&rid) {
        Some(StreamSlot::Done { completion, .. }) => Ok(Completion::from(completion)),
        _ => Err(OpError::UnknownStream { rid }),
    }
}

/// Adopts a call's turn list as canonical (§6.1). A `Ref` turn keeps its
/// existing id; a by-value turn and the completion each get a freshly minted
/// one. Repeatable within a run — the last commit wins (§6.2).
#[op2]
pub async fn op_commit(
    state: Rc<RefCell<OpState>>,
    rid: u32,
    #[serde] options: CommitOptions,
) -> Result<(), OpError> {
    let harness = shared_async(&state).await;

    {
        let harness = harness.borrow();
        if !harness.grant.commit_granted {
            return Err(OpError::CommitNotGranted);
        }
    }

    loop {
        // Inspected *before* calling `advance`, and not just for `Done`/
        // `Failed`: `Missing` (this rid was already committed and removed —
        // a double `commit(call)`) and `Polling` (racing a concurrent
        // `next()`/`.completion` on the same `Call`) both make `advance`
        // return `Err` *without* an `.await` inside it. Swallowing that with
        // `let _ =` and looping again — the previous shape of this code —
        // spun forever: neither state ever becomes terminal on its own, and
        // an `Err` returned before any await point never yields control back
        // to the isolate's event loop, so this was a hang, not a busy-wait.
        let next = {
            let harness = harness.borrow();
            match harness.streams.get(&rid) {
                Some(StreamSlot::Done { .. }) | Some(StreamSlot::Failed { .. }) => None,
                Some(StreamSlot::Polling) => return Err(OpError::StreamBusy { rid }),
                None => return Err(OpError::UnknownStream { rid }),
                // Pending/Active: real progress is possible — `advance`'s
                // only paths from here go through `inner.next().await`.
                Some(_) => Some(()),
            }
        };
        let Some(()) = next else { break };
        // A harness may commit a call it never iterated — drain silently,
        // same as `.completion`. The eventual terminal state (Done or
        // Failed) is what's inspected below; a transient poll error here
        // isn't this op's to report.
        let _ = advance(&harness, rid).await;
    }

    let mut harness = harness.borrow_mut();
    let slot = harness
        .streams
        .remove(&rid)
        .ok_or(OpError::UnknownStream { rid })?;

    let (sent, completion) = match slot {
        StreamSlot::Done { sent, completion } => (sent, completion),
        StreamSlot::Failed {
            sent,
            partial: Some(completion),
            ..
        } if options.partial => (sent, completion),
        StreamSlot::Failed {
            sent,
            partial,
            error,
        } => {
            harness.streams.insert(
                rid,
                StreamSlot::Failed {
                    sent,
                    partial,
                    error,
                },
            );
            return Err(OpError::from(&Refusal::IncompleteCompletion));
        }
        // Unreachable after the drain loop above (it only exits once the
        // slot is Done or Failed) — put back defensively rather than lose a
        // slot to a future change in that loop's behaviour.
        other => {
            harness.streams.insert(rid, other);
            return Err(OpError::UnknownStream { rid });
        }
    };

    let mut lineage = Vec::with_capacity(sent.turns.len() + 1);
    for turn in sent.turns {
        match turn {
            SentTurn::Ref { id } => {
                let message = harness
                    .resolve(&id)
                    .expect("a Sent ref only ever names an id already in the lineage")
                    .1
                    .clone();
                lineage.push(Committed { id, message });
            }
            SentTurn::Value(message) => {
                let id = harness.mint_id();
                lineage.push(Committed { id, message });
            }
        }
    }
    let completion_id = harness.mint_id();
    lineage.push(Committed {
        id: completion_id,
        message: completion.message,
    });

    harness.adopt_lineage(lineage);
    harness.baseline = sent.options;
    // Last commit wins (§6.2), so this always names the call the *current*
    // lineage came from — which is the one the spine has to point at.
    harness.committed_frame = Some(sent.frame);
    harness.recompute_scaffold();

    Ok(())
}

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

/// Records exactly what the harness yielded — H2: the transcript "is the
/// user-facing conversation, not a rendering of one." Takes a raw
/// [`serde_json::Value`] rather than a typed `TranscriptEvent`: this op's job
/// is to record what crossed the boundary, not re-validate it against a Rust
/// enum the harness never sees.
#[op2]
pub fn op_yield(state: &mut OpState, #[serde] event: serde_json::Value) {
    let harness = shared(state);
    let harness = harness.borrow();
    let _ = harness.transcript_tx.send(event);
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// The longest message or field list one `ctx.log` line may carry.
const LOG_TEXT_CAP: usize = 2000;

/// Reports a failed model call on the console. A failed call is otherwise only
/// a frame in the database, and a harness is free to swallow the exception.
fn log_model_failure(harness: &HarnessState, frame: &str, info: &HarnessErrorInfo) {
    tracing::warn!(
        frame,
        model = %harness.grant.model,
        kind = info.kind,
        status = ?info.status,
        request_id = ?info.request_id,
        transient = info.transient,
        error = %info.message,
        "model call failed"
    );
}

/// Bounds harness-supplied text and strips control characters, so a harness
/// cannot forge extra log lines.
fn log_text(text: &str) -> String {
    text.chars()
        .take(LOG_TEXT_CAP)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Writes a harness's line to the console, inside the span of its run.
#[op2]
pub fn op_log(
    state: &mut OpState,
    #[string] level: String,
    #[string] message: String,
    #[serde] fields: serde_json::Value,
) {
    let harness = shared(state);
    {
        let mut harness = harness.borrow_mut();
        if harness.log_budget == 0 {
            return;
        }
        harness.log_budget -= 1;
        if harness.log_budget == 0 {
            tracing::warn!(target: "harness::js", "log budget spent; dropping this run's remaining lines");
            return;
        }
    }

    let message = log_text(&message);
    let fields = if fields.is_null() {
        String::new()
    } else {
        log_text(&fields.to_string())
    };
    match level.as_str() {
        "debug" => tracing::debug!(target: "harness::js", %fields, "{message}"),
        "warn" => tracing::warn!(target: "harness::js", %fields, "{message}"),
        "error" => tracing::error!(target: "harness::js", %fields, "{message}"),
        _ => tracing::info!(target: "harness::js", %fields, "{message}"),
    }
}

deno_core::extension!(
    faber,
    ops = [
        op_capabilities,
        op_tools_available,
        op_functions_available,
        op_function_invoke,
        op_tool_invoke,
        op_history_read,
        op_committed_request,
        op_commit,
        op_llm_stream_open,
        op_llm_stream_next,
        op_llm_stream_completion,
        op_yield,
        op_log,
    ],
);
