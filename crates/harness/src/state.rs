//! Everything a step of the loop needs, held for the run's lifetime.
//!
//! One instance lives behind the mutex a [`Context`](crate::Context) shares
//! (`context.rs`). Everything here is `Send`: a run is a task on the caller's
//! runtime and may move between its threads.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::Stream;
use futures_util::future::BoxFuture;
use llm::{Accumulator, ModelClient, RenderedRequest};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::frame::{CoreEvent, FrameCounter, FrameId};
use crate::mapping::{HarnessErrorInfo, ToolResult as WireToolResult};

/// One message Core holds as part of the canonical lineage, with the id
/// minted for it — either at this run's construction (from a [`Seed`]) or at
/// a `commit` within it.
#[derive(Clone, Debug)]
pub struct Committed {
    pub id: String,
    pub message: llm::Message,
}

/// A turn resolved at `open`: exactly what was sent, either by reference (an
/// id already in the lineage) or by value (new content this call
/// introduced). Carried on a [`StreamSlot`] all the way to its terminal
/// state, because a commit needs exactly what was sent — re-deriving it from
/// the loop's own request would reintroduce the desync ids exist to remove
/// (`proposal.md` §6.1).
#[derive(Clone, Debug)]
pub enum SentTurn {
    Ref { id: String },
    Value(llm::Message),
}

/// What was actually sent for one call: its resolved turns, and the options
/// it was built with. `commit` walks `turns`; a fresh baseline for the next
/// default-path call comes from `options`.
#[derive(Clone, Debug)]
pub struct Sent {
    pub turns: Vec<SentTurn>,
    pub options: Baseline,
    /// The frame this call was opened under. Carried to the terminal state so
    /// a commit can record *which* frame produced the canonical lineage —
    /// a consumer of the frame log (`crates/api`, writing the spine) has to
    /// put the matching exchange on it, and nothing else in the log says
    /// which of several calls won.
    pub frame: FrameId,
}

/// Every option field a request can carry, as the committed lineage was
/// built with it (`proposal.md` §4). `None` means "unset — inherit
/// `crates/llm`'s own default." `tools` is the one exception: it is never
/// optional at this level, because turn 1 with no prior lineage still has a
/// concrete answer (the granted set, `Grant::tools`) — see
/// [`HarnessState::new`].
///
/// `Serialize`/`Deserialize` because a [`Seed`] carries one across runs, and
/// a run's canonical lineage has to survive in storage between them
/// (`history-abstract.md` H7).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub max_tokens: Option<u32>,
    pub tools: Vec<llm::ToolDef>,
    pub tool_choice: Option<llm::ToolChoice>,
    pub thinking: Option<llm::Thinking>,
    pub effort: Option<llm::Effort>,
    pub sampling: Option<llm::Sampling>,
    pub stop_sequences: Option<Vec<String>>,
    pub extra: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The out-of-band head of a request, fingerprinted as three independent
/// hashes rather than one so a `scaffold_mismatch` names which part drifted
/// (`proposal.md` §5.2, §11.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaffoldPrint {
    pub tools: u64,
    /// `None` when the lineage has no leading system run to hash — nothing
    /// to compare a later request's head against yet.
    pub system: Option<u64>,
    pub model: u64,
}

/// What a prior run committed. Nothing in memory survives between runs
/// (`runtime.rs`), so the run for turn N+1 has to be handed this in full.
///
/// Also the *stored* form of a thread's canonical lineage: `crates/api`
/// serializes the [`RunOutcome::committed`](crate::runtime::RunOutcome) of
/// turn N into a blob and deserializes it back as turn N+1's seed. That is
/// the whole of `history-abstract.md` H7, so the type has to round-trip
/// losslessly — `provider` and `model` are here because a lineage is only
/// meaningful against the scope it was built for.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Seed {
    pub provider: String,
    pub model: String,
    pub messages: Vec<llm::Message>,
    pub options: Baseline,
}

/// One in-flight or finished model call, keyed by the id its
/// [`Call`](crate::Call) holds.
pub enum StreamSlot {
    /// Rendered but not yet sent: a request is not sent until its call is
    /// polled. Rendering already happened at `open`, so
    /// a validation refusal surfaces synchronously there rather than as a
    /// deferred stream error, and the frame log records request bytes at
    /// open — J1 done at the right point.
    Pending {
        rendered: RenderedRequest,
        sent: Sent,
    },
    /// Sent; `inner` yields the provider's raw events, `accumulator` folds
    /// them so the terminal step can produce a `Completion` (for the fold
    /// invariant, `harness-events.md` §8) and, on `commit`, adopt it.
    Active {
        inner: Pin<Box<dyn Stream<Item = llm::Result<llm::Event>> + Send>>,
        accumulator: Accumulator,
        sent: Sent,
    },
    /// Finished cleanly. A commit and a completion read both read from here.
    Done {
        sent: Sent,
        completion: llm::Completion,
    },
    /// Finished with an error; already reported to the caller once.
    Failed {
        sent: Sent,
        /// The folded completion, when the accumulator managed to fold one
        /// before the failure — absent when `Accumulator::finish` itself is
        /// what failed (e.g. truncated tool-call JSON), since that consumes
        /// the accumulated state with no way to recover it. A partial
        /// commit needs `Some` here to have anything to adopt.
        partial: Option<llm::Completion>,
        error: HarnessErrorInfo,
    },
    /// Left behind while an `advance()` call for this `rid` is between
    /// removing the slot and re-inserting it (i.e. across its `.await`).
    /// A second `advance()` landing on this — a completion read racing a
    /// concurrent `next()` on the same `Call`, or two overlapping
    /// iterations — gets a clear [`crate::error::Error::StreamBusy`]
    /// instead of a misleading "unknown stream": the slot isn't missing,
    /// it's busy. Concurrent polling of *one* `Call` isn't supported;
    /// concurrent polling of *different* `Call`s (distinct `rid`s — the
    /// best-of-N case `commit` exists for) is unaffected, since each has its
    /// own entry.
    Polling,
}

/// Dispatches a granted tool by name. Only ever called for a tool already
/// confirmed present in `Grant::tools` — "not offered" is answered in
/// [`Context::invoke_tool`](crate::Context::invoke_tool) before this is
/// reached: an absent tool is a move the loop doesn't have, not a failure.
pub type ToolInvoker = Arc<
    dyn Fn(String, serde_json::Value) -> BoxFuture<'static, Result<WireToolResult, String>>
        + Send
        + Sync,
>;

pub type DynamicFunction = Arc<
    dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, String>>
        + Send
        + Sync,
>;

#[derive(Clone, Default)]
pub struct FunctionRegistry {
    functions: Arc<HashMap<String, DynamicFunction>>,
}

impl FunctionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<F>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(serde_json::Value) -> BoxFuture<'static, Result<serde_json::Value, String>>
            + Send
            + Sync
            + 'static,
    {
        Arc::make_mut(&mut self.functions).insert(name.into(), Arc::new(function));
    }

    pub(crate) fn get(&self, name: &str) -> Option<DynamicFunction> {
        self.functions.get(name).cloned()
    }
}

/// The reasoning half of a request, resolved by the caller for a whole run.
///
/// The two fields are orthogonal: `thinking` says whether to
/// reason and how much of it comes back, `effort` how much to spend. They
/// travel together because one selection produces both.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reasoning {
    pub thinking: Option<llm::Thinking>,
    pub effort: Option<llm::Effort>,
}

/// What this run was granted — the entire perimeter (`abstract.md` §4's "no
/// ambient authority"). Whatever isn't set here, the loop cannot reach.
pub struct Grant {
    pub client: Arc<dyn ModelClient>,
    pub model: String,
    /// How much of a replayed assistant turn's reasoning this model wants back
    /// (`llm::ReasoningHistory`). Granted rather than proposed: which endpoint
    /// accepts what is a fact about the model the caller resolved, not a
    /// decision the loop gets to make per call, so it sits beside `model`
    /// rather than in the [`Baseline`] a call can override. `None` leaves
    /// it to the wire's default.
    pub reasoning_history: Option<llm::ReasoningHistory>,
    /// The reasoning this run was told to do — the caller's own resolution of
    /// whatever the user picked against what the model offers.
    ///
    /// `None` is a caller with no opinion, and leaves the committed lineage's
    /// own `thinking`/`effort` to carry across runs like every other option
    /// field. `Some` replaces them for this run, `None` fields included: the
    /// selection is made per run and can change between two of them, so a
    /// baseline inherited from turn N would otherwise pin turn N+1 to a knob
    /// the user has since moved. A call that sets either field itself
    /// still wins — see [`Baseline`] and `Context::stream`.
    pub reasoning: Option<Reasoning>,
    /// Per-endpoint request tweaks (`llm::AdvancedOptions`), applied as a
    /// floor beneath every call's own `extra` — see `Context::stream`.
    /// Granted for the same reason as `reasoning_history`: a fact about the
    /// endpoint behind `model`, not a per-call decision.
    pub advanced_options: llm::AdvancedOptions,
    pub tools: Vec<llm::ToolDef>,
    pub tool_invoker: Option<ToolInvoker>,
    pub commit_granted: bool,
    pub functions: FunctionRegistry,
    /// How a stop asked for from outside reaches this run
    /// ([`crate::interrupt`]). `None` is a run nobody can stop, which is the
    /// right default for a caller that has nowhere to put the other half.
    pub interrupt: Option<crate::interrupt::Interrupt>,
}

pub struct HarnessState {
    pub grant: Grant,

    pub streams: HashMap<u32, StreamSlot>,
    next_stream_id: u32,

    /// The canonical lineage: Core's ground truth, in order. Every element
    /// carries the id minted for it, either at construction (from the
    /// [`Seed`]) or at a `commit` within this run.
    pub committed_messages: Vec<Committed>,
    index_by_id: HashMap<String, usize>,
    next_message_id: u32,

    /// Every option field the committed lineage was built with — §4's
    /// default path, and `Context::committed_request`'s answer. The single
    /// stored copy both that and `Context::stream`'s merge read, so the two
    /// can't independently drift.
    pub baseline: Baseline,
    /// The out-of-band head fingerprint of the committed lineage, recomputed
    /// on every commit ([`HarnessState::recompute_scaffold`]).
    pub scaffold: ScaffoldPrint,
    /// The frame of the call whose commit produced the current lineage.
    /// `None` until something commits — a loop that streams and never
    /// commits leaves the lineage exactly as it was seeded, and there is no
    /// exchange for the spine to point at. Last commit wins (§6.2), so this
    /// tracks the *latest* one, matching `committed_messages`.
    pub committed_frame: Option<FrameId>,

    pub frame_counter: FrameCounter,

    /// Whatever the loop emitted, in order: the user-facing conversation
    /// itself, not a rendering of one. Raw JSON, because its consumers store
    /// and stream it as JSON.
    pub transcript_tx: UnboundedSender<serde_json::Value>,
    pub frames_tx: UnboundedSender<CoreEvent>,
}

impl HarnessState {
    pub fn new(
        grant: Grant,
        seed: Seed,
        transcript_tx: UnboundedSender<serde_json::Value>,
        frames_tx: UnboundedSender<CoreEvent>,
    ) -> Self {
        let mut committed_messages = Vec::with_capacity(seed.messages.len());
        let mut index_by_id = HashMap::new();
        let mut next_message_id: u32 = 0;
        for message in seed.messages {
            let id = format!("m{next_message_id}");
            next_message_id += 1;
            index_by_id.insert(id.clone(), committed_messages.len());
            committed_messages.push(Committed { id, message });
        }

        // Turn 1's default path (`proposal.md` §4): no prior lineage means
        // no prior tool override either, so the baseline is the granted set.
        // A seeded run's baseline already carries forward whatever the prior
        // run's own default resolved to, so this only ever fires once, at
        // the start of the very first turn.
        let mut baseline = seed.options;
        if baseline.tools.is_empty() {
            baseline.tools = grant.tools.clone();
        }

        // Unlike tools, this applies on every turn and not only the first:
        // the caller resolved it for *this* run from a selection the user can
        // move between turns, so what turn N committed is not evidence about
        // what turn N+1 should default to. Assigned whole — a `None` field in
        // a granted `Reasoning` means "send nothing", which is exactly what
        // clearing the knob has to be able to say.
        if let Some(reasoning) = grant.reasoning {
            baseline.thinking = reasoning.thinking;
            baseline.effort = reasoning.effort;
        }

        let mut state = Self {
            grant,
            streams: HashMap::new(),
            next_stream_id: 0,
            committed_messages,
            index_by_id,
            next_message_id,
            baseline,
            scaffold: ScaffoldPrint {
                tools: 0,
                system: None,
                model: 0,
            },
            committed_frame: None,
            frame_counter: FrameCounter::default(),
            transcript_tx,
            frames_tx,
        };
        state.recompute_scaffold();
        state
    }

    pub fn insert_stream(&mut self, slot: StreamSlot) -> u32 {
        let id = self.next_stream_id;
        self.next_stream_id += 1;
        self.streams.insert(id, slot);
        id
    }

    /// Mints the next run-scoped message id. Monotonic, never reused, even
    /// when a later commit drops the message it named from the lineage.
    pub fn mint_id(&mut self) -> String {
        let id = format!("m{}", self.next_message_id);
        self.next_message_id += 1;
        id
    }

    /// Resolves a committed id to its lineage position and stored content.
    pub fn resolve(&self, id: &str) -> Option<(usize, &llm::Message)> {
        let &index = self.index_by_id.get(id)?;
        Some((index, &self.committed_messages[index].message))
    }

    /// Atomically replaces the canonical lineage and rebuilds the id index
    /// alongside it — the two must never carry different generations.
    pub fn adopt_lineage(&mut self, messages: Vec<Committed>) {
        self.index_by_id = messages
            .iter()
            .enumerate()
            .map(|(index, committed)| (committed.id.clone(), index))
            .collect();
        self.committed_messages = messages;
    }

    /// A borrowed view of the canonical lineage, in order — for anything
    /// that needs "just the messages", without ids attached.
    pub fn lineage_iter(&self) -> impl Iterator<Item = &llm::Message> + Clone {
        self.committed_messages
            .iter()
            .map(|committed| &committed.message)
    }

    /// Recomputes [`Self::scaffold`] from the current `baseline.tools`,
    /// lineage head, and grant — called once at construction and again after
    /// every commit, since a commit can change the tool baseline and always
    /// changes the lineage.
    pub fn recompute_scaffold(&mut self) {
        self.scaffold = crate::scaffold::print_for(
            &self.baseline.tools,
            self.lineage_iter(),
            self.grant.client.provider(),
            &self.grant.model,
        );
    }
}
