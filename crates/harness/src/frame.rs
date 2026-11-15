//! The frame log: what happened, recorded at the capability boundary.
//!
//! `harness-events.md` §4. Built inside [`Context`](crate::Context)'s own
//! steps only — never from anything the loop reports about itself (J1,
//! `history-abstract.md`). It exists for persistence and for a UI to say
//! what a running loop is doing.

use serde_json::Value;

use crate::mapping::HarnessErrorInfo;

/// Path-structured, deterministic — minted inside the step that starts a
/// frame, under the run's lock, so two calls opened concurrently still get
/// ids in the order they were opened (`harness-events.md` §4, §8).
pub type FrameId = String;

#[derive(Debug, Clone)]
pub enum FrameDetail {
    Harness,
    Model { model: String },
    Tool { name: String, input: Value },
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Ok,
    Failed { error: HarnessErrorInfo },
}

#[derive(Debug, Clone)]
pub enum CoreEvent {
    FrameStart {
        frame: FrameId,
        parent: Option<FrameId>,
        detail: FrameDetail,
    },
    /// The request bytes exactly as rendered, recorded at `open` — before
    /// dispatch, and before the loop can have done anything with the call.
    /// J1 (`history-abstract.md`): ground truth is recorded at the capability
    /// boundary, never reported by the loop, so the record cannot disagree
    /// with what was sent.
    ///
    /// Emitted for every model frame, including calls that are opened and
    /// never polled — `exchange.request_blob_digest` is `NOT NULL`, and H7's
    /// "garbage class" of unreferenced exchanges is the intended home for
    /// the ones that went nowhere.
    ModelRequest {
        frame: FrameId,
        body: Vec<u8>,
    },
    ModelEvent {
        frame: FrameId,
        event: llm::Event,
    },
    ToolResult {
        frame: FrameId,
        content: String,
        is_error: bool,
    },
    /// The folded usage for a completed model frame, emitted immediately
    /// before `FrameStop` on both the clean and the failed path — a
    /// truncated call still burned cache-write tokens, and `harness-events.md`
    /// §8 exists to measure spend, not just success. Redundant with the fold
    /// invariant by construction (a consumer could re-derive this by folding
    /// the frame's `ModelEvent`s through `Accumulator`), and that's the
    /// point: a log consumer shouldn't have to run an accumulator just to
    /// get a cache-read ratio.
    ModelUsage {
        frame: FrameId,
        usage: llm::Usage,
    },
    FrameStop {
        frame: FrameId,
        outcome: Outcome,
    },
    #[allow(
        dead_code,
        reason = "nothing emits a mark yet; kept for the log's own vocabulary"
    )]
    Mark {
        frame: FrameId,
        label: String,
        data: Option<Value>,
    },
}

/// Mints flat child ids under the root harness frame (`"0"`): `"0.1"`,
/// `"0.2"`, ... Nested frames (a tool invoking its own sub-harness) are out
/// of scope for this slice — there is no escalation surface (§7 rejected).
#[derive(Debug, Default)]
pub struct FrameCounter {
    next: u32,
}

impl FrameCounter {
    pub const ROOT: &'static str = "0";

    pub fn next_child(&mut self) -> FrameId {
        self.next += 1;
        format!("{}.{}", Self::ROOT, self.next)
    }
}
