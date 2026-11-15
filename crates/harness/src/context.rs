//! What the loop is handed: every outbound step a run can take.
//!
//! A [`Context`] is the whole of a run's reach. A model call, a tool, a
//! granted function, the committed history and the transcript all go through
//! it, and each step that reaches outward is recorded in the frame log here,
//! at the step, rather than reported by the loop afterwards.
//!
//! The state behind it is shared through a mutex that is never held across
//! an `.await`: every async step takes what it needs as owned values, drops
//! the lock, awaits, and locks again to store the result.

use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use futures_util::{Stream, StreamExt};
use llm::{Accumulator, ModelClient, RenderedRequest};

use crate::error::Error;
use crate::frame::{CoreEvent, FrameDetail, Outcome};
use crate::mapping::{
    CommitOptions, Completion, HarnessErrorInfo, LLMEvent, LLMRequest, Message, RequestOptions,
    ToolDef, ToolResult,
};
use crate::state::{Baseline, Committed, HarnessState, Sent, SentTurn, StreamSlot};
use crate::validate::Refusal;

/// A run's capabilities. Cheap to clone; every clone reaches the same run.
#[derive(Clone)]
pub struct Context {
    state: Arc<Mutex<HarnessState>>,
}

/// One model call, opened by [`Context::stream`].
///
/// Nothing is sent until the call is first polled, through [`Call::next`] or
/// [`Call::completion`].
pub struct Call {
    context: Context,
    rid: u32,
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

impl Context {
    pub(crate) fn new(state: Arc<Mutex<HarnessState>>) -> Self {
        Self { state }
    }

    fn lock(&self) -> MutexGuard<'_, HarnessState> {
        // A panic while the lock is held leaves the state as it was at the
        // last completed step, which is still the truth the outcome reports.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // -----------------------------------------------------------------------
    // Tools and functions
    // -----------------------------------------------------------------------

    /// The tools this run was granted.
    pub fn tools(&self) -> Vec<ToolDef> {
        self.lock().grant.tools.iter().map(ToolDef::from).collect()
    }

    /// Invokes a granted tool. `Ok(None)` means the tool was never granted,
    /// which is not the same as tried and failed: it is a move this run does
    /// not have.
    pub async fn invoke_tool(
        &self,
        name: &str,
        input: serde_json::Value,
    ) -> Result<Option<ToolResult>, Error> {
        let (invocation, interrupt) = {
            let harness = self.lock();
            if !harness.grant.tools.iter().any(|tool| tool.name == name) {
                return Ok(None);
            }
            let Some(invoker) = harness.grant.tool_invoker.as_ref() else {
                return Err(Error::UnknownTool {
                    name: name.to_owned(),
                });
            };
            // Checked before dispatch as well as during it: a stop that landed
            // while the loop was between calls must not be answered by
            // starting fresh work.
            if stopped(&harness) {
                return Err(HarnessErrorInfo::cancelled().into());
            }
            (
                (invoker)(name.to_owned(), input),
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

        // A tool failure is otherwise text only the model reads. Info, because
        // many are the model's own mistakes; a dispatch failure never is.
        match dispatched {
            Ok(result) => {
                if result.is_error {
                    tracing::info!(tool = %name, error = %log_text(&result.content), "tool call failed");
                }
                Ok(Some(ToolResult {
                    content: result.content,
                    is_error: result.is_error,
                }))
            }
            Err(message) => {
                tracing::warn!(tool = %name, error = %message, "tool call could not be dispatched");
                Err(Error::ToolDispatchFailed {
                    name: name.to_owned(),
                    message,
                })
            }
        }
    }

    /// Whether the caller registered a function under `name` for this run.
    pub fn has_function(&self, name: &str) -> bool {
        self.lock().grant.functions.get(name).is_some()
    }

    /// Invokes a function the caller registered for this run.
    pub async fn invoke_function(
        &self,
        name: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, Error> {
        let function =
            self.lock()
                .grant
                .functions
                .get(name)
                .ok_or_else(|| Error::UnknownFunction {
                    name: name.to_owned(),
                })?;

        // Logged here rather than left to the loop: the loop may treat a
        // failure as best effort and carry on, and the operator still needs
        // to know it happened.
        function(input).await.map_err(|message| {
            tracing::warn!(function = %name, error = %message, "granted function failed");
            Error::FunctionFailed {
                name: name.to_owned(),
                message,
            }
        })
    }

    // -----------------------------------------------------------------------
    // History
    // -----------------------------------------------------------------------

    /// The committed lineage, each message carrying the id minted for it.
    /// Exact, not reconstructed: the stored messages are the ground truth.
    pub fn history(&self) -> Vec<Message> {
        self.lock()
            .committed_messages
            .iter()
            .map(|committed| Message::committed(&committed.id, &committed.message))
            .collect()
    }

    /// The committed lineage's own request options (`proposal.md` §4): the
    /// exact baseline [`Context::stream`] fills unset fields from. Both read
    /// the same stored `Baseline`, so they cannot drift apart.
    pub fn committed_request(&self) -> RequestOptions {
        RequestOptions::from(&self.lock().baseline)
    }

    // -----------------------------------------------------------------------
    // Model calls
    // -----------------------------------------------------------------------

    /// Merges the request against the committed baseline, validates it
    /// (`proposal.md` §7), renders, and opens a call — but does not send.
    /// Rendering here rather than at the first poll means a refusal surfaces
    /// at once, and the frame log records the request bytes the moment the
    /// call is opened, not after the network round trip starts.
    pub fn stream(&self, request: LLMRequest) -> Result<Call, Error> {
        let mut harness = self.lock();

        // Refused before anything is rendered or logged. A loop may handle a
        // `cancelled` failure and keep going — that is what makes it an
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
        .map_err(|refusal| Error::from(&refusal))?;

        let mut llm_messages = Vec::with_capacity(sent_turns.len());
        for turn in &sent_turns {
            let message = match turn {
                SentTurn::Ref { id } => harness
                    .resolve(id)
                    .expect(
                        "validate::check only ever returns ids it resolved against this lineage",
                    )
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
        // call's own `extra` overrides an unrelated key entirely (`or_else`
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
            .map_err(|error| Error::from(&error))?;

        let frame = harness.frame_counter.next_child();
        let model = harness.grant.model.clone();
        let _ = harness.frames_tx.send(CoreEvent::FrameStart {
            frame: frame.clone(),
            parent: Some(crate::frame::FrameCounter::ROOT.to_string()),
            detail: FrameDetail::Model { model },
        });
        // Immediately after the frame opens and before the slot exists, so the
        // bytes are logged whether or not this call is ever polled.
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
        Ok(Call {
            context: self.clone(),
            rid,
        })
    }

    /// Advances stream `rid` by exactly one event, updating its slot. Shared
    /// by [`Call::next`], which hands each event to the loop, and
    /// [`Call::completion`] and [`Context::commit`], which drain silently when
    /// asked before the loop finished iterating.
    async fn advance(&self, rid: u32) -> Result<Option<LLMEvent>, Error> {
        let (mut inner, mut accumulator, sent, interrupt) = {
            let mut state = self.lock();
            let slot = state
                .streams
                .remove(&rid)
                .ok_or(Error::UnknownStream { rid })?;
            let interrupt = state.grant.interrupt.clone();
            match slot {
                // Another `advance()` for this rid is between removing its slot
                // and re-inserting it (i.e. mid-`.await`). Put the marker back
                // and reject clearly, rather than letting this call also see
                // "missing" and misreport `UnknownStream` on a live stream.
                StreamSlot::Polling => {
                    state.streams.insert(rid, StreamSlot::Polling);
                    return Err(Error::StreamBusy { rid });
                }
                StreamSlot::Pending { rendered, sent } => {
                    let client = state.grant.client.clone();
                    state.streams.insert(rid, StreamSlot::Polling);
                    let _ = state.frames_tx.send(CoreEvent::ModelDispatch {
                        frame: sent.frame.clone(),
                        at: Instant::now(),
                    });
                    (
                        owned_stream(client, rendered),
                        Accumulator::new(),
                        sent,
                        interrupt,
                    )
                }
                StreamSlot::Active {
                    inner,
                    accumulator,
                    sent,
                } => {
                    state.streams.insert(rid, StreamSlot::Polling);
                    (inner, accumulator, sent, interrupt)
                }
                StreamSlot::Done { sent, completion } => {
                    state
                        .streams
                        .insert(rid, StreamSlot::Done { sent, completion });
                    return Ok(None);
                }
                // Re-polling an already-failed stream returns its *original*
                // failure, not a misleading "unknown stream" — a completion
                // read on a call that broke mid-iteration must see the real
                // reason.
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

        let arrived = Instant::now();

        let Some(next) = next else {
            // Terminal, and shaped exactly like a stream that broke: whatever the
            // accumulator folded is kept, so a partial commit (§6.3) still has
            // something to adopt, and the frame stops failed because this call
            // never produced a completion. Dropping `inner` is what closes the
            // provider connection.
            let info = HarnessErrorInfo::cancelled();
            let partial = accumulator.finish().ok();
            let mut state = self.lock();
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
                let mut state = self.lock();
                let _ = state.frames_tx.send(CoreEvent::ModelEvent {
                    frame: frame.clone(),
                    event,
                    at: arrived,
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
                let mut state = self.lock();
                log_model_failure(&state, &frame, &info);
                // Whatever the accumulator folded before the stream broke is
                // recovered here, best-effort, so a partial commit (§6.3) has
                // something to adopt.
                let partial = accumulator.finish().ok();
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
                let mut state = self.lock();
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

    /// Whether this run may [`commit`](Context::commit).
    pub fn can_commit(&self) -> bool {
        self.lock().grant.commit_granted
    }

    /// Adopts a call's turn list as canonical (`proposal.md` §6.1). A turn
    /// sent by reference keeps its id; a turn sent by value and the
    /// completion each get a freshly minted one. Repeatable within a run —
    /// the last commit wins (§6.2).
    pub async fn commit(&self, call: &Call, options: CommitOptions) -> Result<(), Error> {
        let rid = call.rid;

        if !self.lock().grant.commit_granted {
            return Err(Error::CommitNotGranted);
        }

        loop {
            // Inspected *before* calling `advance`, and not just for `Done`/
            // `Failed`: a missing slot (this call was already committed and
            // removed — a double commit) and `Polling` (racing a concurrent
            // poll of the same call) both make `advance` return `Err` without
            // ever awaiting. Ignoring that and looping again would spin
            // forever: neither state becomes terminal on its own.
            let progress = {
                let harness = self.lock();
                match harness.streams.get(&rid) {
                    Some(StreamSlot::Done { .. }) | Some(StreamSlot::Failed { .. }) => false,
                    Some(StreamSlot::Polling) => return Err(Error::StreamBusy { rid }),
                    None => return Err(Error::UnknownStream { rid }),
                    // Pending/Active: real progress is possible — `advance`'s
                    // only paths from here go through `inner.next().await`.
                    Some(_) => true,
                }
            };
            if !progress {
                break;
            }
            // A loop may commit a call it never iterated — drain silently,
            // same as a completion read. The eventual terminal state (Done or
            // Failed) is what's inspected below; a poll error here isn't this
            // step's to report.
            let _ = self.advance(rid).await;
        }

        let mut harness = self.lock();
        let slot = harness
            .streams
            .remove(&rid)
            .ok_or(Error::UnknownStream { rid })?;

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
                return Err(Error::from(&Refusal::IncompleteCompletion));
            }
            // Unreachable after the drain loop above (it only exits once the
            // slot is Done or Failed) — put back defensively rather than lose a
            // slot to a future change in that loop's behaviour.
            other => {
                harness.streams.insert(rid, other);
                return Err(Error::UnknownStream { rid });
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

    // -----------------------------------------------------------------------
    // Transcript
    // -----------------------------------------------------------------------

    /// Adds one event to the transcript: the user-facing conversation, in the
    /// order the loop produced it.
    pub fn emit(&self, event: serde_json::Value) {
        let _ = self.lock().transcript_tx.send(event);
    }

    /// [`emit`](Context::emit), for a model event.
    pub fn emit_model_event(&self, event: &LLMEvent) {
        match serde_json::to_value(event) {
            Ok(value) => self.emit(value),
            Err(error) => tracing::error!(%error, "a model event did not serialize"),
        }
    }
}

impl Call {
    /// The next event of this call, or `None` once it has ended cleanly.
    ///
    /// A failed call returns the failure that actually happened, and keeps
    /// returning it.
    pub async fn next(&self) -> Result<Option<LLMEvent>, Error> {
        self.context.advance(self.rid).await
    }

    /// Drains the call to its end and returns the completion. A failed call
    /// returns the failure that actually happened, whether this read is what
    /// reached it or it had already happened.
    pub async fn completion(&self) -> Result<Completion, Error> {
        let rid = self.rid;
        loop {
            let is_done = matches!(
                self.context.lock().streams.get(&rid),
                Some(StreamSlot::Done { .. })
            );
            if is_done {
                break;
            }
            self.context.advance(rid).await?;
        }

        match self.context.lock().streams.get(&rid) {
            Some(StreamSlot::Done { completion, .. }) => Ok(Completion::from(completion)),
            _ => Err(Error::UnknownStream { rid }),
        }
    }
}

fn owned_stream(
    client: Arc<dyn ModelClient>,
    rendered: RenderedRequest,
) -> Pin<Box<dyn Stream<Item = llm::Result<llm::Event>> + Send>> {
    Box::pin(async_stream::stream! {
        let stream = client.send(rendered);
        futures_util::pin_mut!(stream);
        while let Some(item) = stream.next().await {
            yield item;
        }
    })
}

/// The longest tool error one log line may carry.
const LOG_TEXT_CAP: usize = 2000;

/// Reports a failed model call on the console. A failed call is otherwise only
/// a frame in the database, and the loop is free to retry past it.
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

/// Bounds tool-supplied text and strips control characters, so a tool's
/// output cannot forge extra log lines.
fn log_text(text: &str) -> String {
    text.chars()
        .take(LOG_TEXT_CAP)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
