//! The draft session event extension: a plugin that requests the `events`
//! capability is told, live, everything that happens in the sessions it is
//! bound in.
//!
//! There is no event vocabulary here on purpose. An event is what the core's
//! own session stream carries, the one the UI renders: a free-form `kind` and
//! its JSON `payload`, under the `(run_id, seq)` cursor that orders it. That
//! is the user's turn, reasoning and text as they stream, whole messages once
//! they close, tool calls and their results, the run's end, and the session's
//! notes about plugins and environments. A kind the core adds later reaches
//! plugins without a change here.
//!
//! Delivery is asynchronous: the run never waits on a plugin, and a plugin
//! that falls behind loses live events rather than slowing anything down.
//! What it lost is announced by a [`LAGGED`] event, and the persisted events
//! among them (whole messages, tool results, run markers; never the deltas)
//! are delivered again after it. A server restart does the same. So a plugin
//! sees every persisted event at least once and should drop one it has seen
//! by its `(run_id, seq)`.

use serde::{Deserialize, Serialize};

use crate::types::Json;

/// One event of a session, as the session stream carries it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// The run it belongs to. Runs of a session are ordered by id.
    pub run_id: String,
    /// Its place in the run; unique within the run, not the session.
    pub seq: i64,
    /// What it is, as the core names it: `input`, `message`, `run_end`, or a
    /// streamed event's own `type`, such as `block_delta`.
    pub kind: String,
    pub payload: Json,
}

/// The kind of the event that says live events were lost: its `payload` is
/// `{"dropped": <count>}`, and its `(run_id, seq)` are those of the last event
/// delivered before the loss. The persisted events lost follow it.
pub const LAGGED: &str = "lagged";
