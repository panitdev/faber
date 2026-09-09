//! Provider-neutral request and content types.
//!
//! Nothing here is Anthropic-shaped on purpose: a provider module owns the
//! translation to and from its own wire format. The one concession to reality
//! is `ToolDef::input_schema`, which is JSON Schema because every provider
//! that supports tools speaks it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who authored a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    /// An operator instruction. Providers that support it accept this role
    /// anywhere in `messages`, not just as a leading turn — see
    /// [`Message::system`].
    System,
}

/// One turn in a conversation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    /// An operator instruction, as its own turn. A leading run of these
    /// becomes a provider's top-level system prompt; one appearing later
    /// becomes a mid-conversation system message on providers that support
    /// it. See [`Role::System`].
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// Concatenation of every text block, ignoring thinking and tool traffic.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// The length of the maximal prefix of [`Role::System`] turns — the run a
/// provider hoists into a top-level system prompt (see [`Message::system`]).
///
/// Pure and provider-neutral, so anything that needs the same rule a wire
/// module uses to decide what's "the system prompt" versus "a mid-conversation
/// system message" gets it from one place rather than reimplementing
/// `take_while`.
pub fn leading_system_run<'a, I>(messages: I) -> usize
where
    I: IntoIterator<Item = &'a Message>,
{
    messages
        .into_iter()
        .take_while(|message| message.role == Role::System)
        .count()
}

/// A piece of a message.
///
/// `Unknown` exists so a provider that grows a block type we don't model yet
/// round-trips it rather than dropping it — the contract may break across
/// major versions, but a block that arrives must be one that can be sent back.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Model reasoning. `signature` is opaque and must be preserved verbatim
    /// when the block is sent back to the same model.
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Unknown {
        raw: Value,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn tool_error(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// One entry in a request's message list: an ordinary message, or a
/// previously-rendered prefix spliced back in by reference.
///
/// A [`Turn::Span`] may appear only at index 0. A span names a *prefix* —
/// H5 frames editing as "send the prefix as a span and the edited tail by
/// value" — so it can never be anything but the front of the array; a span
/// found elsewhere is a [`crate::Error::SpanPosition`].
///
/// Not `Serialize`/`Deserialize`: nothing here crosses the wire as JSON.
/// Wire modules build provider bytes directly from this by matching on it
/// (see `crate::span`), and a [`RenderedSpan`](crate::RenderedSpan)'s
/// content must never round-trip through a parsed value tree (H5).
#[derive(Clone, Debug)]
pub enum Turn {
    Value(Message),
    Span(crate::span::RenderedSpan),
}

impl From<Message> for Turn {
    fn from(message: Message) -> Self {
        Turn::Value(message)
    }
}

/// A tool the model may call. Core carries the declaration; dispatch belongs
/// to whatever the workflow handed the harness.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments.
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

/// Reasoning configuration. Providers that don't reason ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Thinking {
    /// The model decides when and how much to think.
    Adaptive {
        display: ThinkingDisplay,
    },
    Disabled,
}

/// How much of an assistant turn's reasoning goes back to the provider when
/// that turn is replayed as history.
///
/// Providers disagree, and not only across wires: one endpoint verifies a
/// signature and rejects reasoning without it, the next rejects reasoning
/// altogether, and a third accepts either. That is a property of the model
/// behind the URL, not of the wire, so it is a setting rather than something
/// this crate decides. Left unset on a [`Request`], each wire keeps the
/// behaviour it has always had — see [`crate::anthropic::DEFAULT_REASONING`]
/// and [`crate::openai::DEFAULT_REASONING`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningHistory {
    /// Reasoning text and its signature both go back, verbatim. What a model
    /// that verifies its own reasoning requires.
    Full,
    /// Reasoning text goes back; the signature does not. On a wire with no
    /// signature field this is indistinguishable from [`Self::Full`].
    Text,
    /// No reasoning goes back. Thinking blocks are dropped from the rendered
    /// history; an assistant turn left with nothing else is dropped whole.
    Omitted,
}

/// Per-endpoint request tweaks a model config wants applied to every call it
/// makes — a property of what's behind the `base_url`, not of the wire, so it
/// sits beside [`ReasoningHistory`] as something the caller resolves rather
/// than something a workflow decides per call.
///
/// `Default` is the no-op value: nothing here changes the request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AdvancedOptions {
    /// Some OpenAI-wire endpoints (MiniMax among them) mix reasoning into
    /// `content` deltas unless told otherwise. Setting this sends
    /// `reasoning_split: true` in the request body, which switches those
    /// endpoints to the `reasoning_content` delta every wire already parses.
    #[serde(default)]
    pub reasoning_split: bool,
    /// Merged into the request body verbatim, beneath whatever a call's own
    /// [`Request::extra`] supplies — for endpoint quirks this crate has no
    /// dedicated field for.
    #[serde(default)]
    pub extra: serde_json::Map<String, Value>,
}

impl AdvancedOptions {
    /// The floor a call's own `extra` is layered over — see
    /// `Request::extra`'s "modelled fields win" rule, which this predates.
    pub fn as_extra(&self) -> serde_json::Map<String, Value> {
        let mut map = self.extra.clone();
        if self.reasoning_split {
            map.entry("reasoning_split".to_string())
                .or_insert(Value::Bool(true));
        }
        map
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingDisplay {
    /// Thinking blocks arrive with empty text.
    Omitted,
    /// Thinking blocks carry a readable summary.
    Summarized,
}

/// How much the model should spend on a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// Sampling knobs, all optional.
///
/// Current Anthropic models reject non-default `temperature`/`top_p`/`top_k`
/// outright, so a `None` here means "omit the field", not "send the default".
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Sampling {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
}

impl Sampling {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none() && self.top_p.is_none() && self.top_k.is_none()
    }
}

/// A single model call.
///
/// `model` is a plain string: roles are an indirection the workflow resolves
/// before anything reaches this crate.
///
/// Not `Serialize`/`Deserialize` — `messages` carries a [`Turn::Span`] whose
/// content is provider-rendered bytes, not a value tree to round-trip.
#[derive(Clone, Debug)]
pub struct Request {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<Turn>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: Option<ToolChoice>,
    pub thinking: Option<Thinking>,
    pub effort: Option<Effort>,
    /// How much of a replayed assistant turn's reasoning to send back.
    /// `None` leaves it to the wire module's default.
    pub reasoning_history: Option<ReasoningHistory>,
    pub sampling: Sampling,
    pub stop_sequences: Vec<String>,
    /// Fields merged into the request body verbatim, for provider features
    /// this crate has no opinion about yet. Keys that collide with fields
    /// above are ignored.
    pub extra: serde_json::Map<String, Value>,
}

/// Enough headroom that a normal answer isn't truncated; a harness that cares
/// sets its own.
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;

impl Request {
    /// The zero-configuration call: a model and a conversation.
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            messages: messages.into_iter().map(Turn::Value).collect(),
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            effort: None,
            reasoning_history: None,
            sampling: Sampling::default(),
            stop_sequences: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }
}

/// Why the model stopped.
///
/// Surfaced, never acted on: resuming a `PauseTurn` or falling back on a
/// `Refusal` is a decision about the shape of the loop, which is the harness's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    ContextWindowExceeded,
    Other(String),
}

/// Detail attached to a refusal. Absent for every other stop reason.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StopDetails {
    pub category: Option<String>,
    pub explanation: Option<String>,
}

/// Token accounting for one call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub reasoning_tokens: u64,
}

impl Usage {
    /// Applies a partial report, overwriting exactly the fields it carries.
    ///
    /// A stream reports input counts up front and output counts at the end,
    /// so a call's totals are assembled from several partial reports. A
    /// reported zero overwrites like any other value — a refusal declined
    /// before any output really did produce zero output tokens, and recording
    /// that as "unreported" would be a lie about what happened.
    pub fn apply(&mut self, delta: &UsageDelta) {
        if let Some(count) = delta.input_tokens {
            self.input_tokens = count;
        }
        if let Some(count) = delta.output_tokens {
            self.output_tokens = count;
        }
        if let Some(count) = delta.cache_read_input_tokens {
            self.cache_read_input_tokens = count;
        }
        if let Some(count) = delta.cache_creation_input_tokens {
            self.cache_creation_input_tokens = count;
        }
        if let Some(count) = delta.reasoning_tokens {
            self.reasoning_tokens = count;
        }
    }
}

/// One provider report of token usage. `None` means the field was absent from
/// that report, which is not the same as it being zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageDelta {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

impl From<Usage> for UsageDelta {
    fn from(usage: Usage) -> Self {
        Self {
            input_tokens: Some(usage.input_tokens),
            output_tokens: Some(usage.output_tokens),
            cache_read_input_tokens: Some(usage.cache_read_input_tokens),
            cache_creation_input_tokens: Some(usage.cache_creation_input_tokens),
            reasoning_tokens: Some(usage.reasoning_tokens),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_has_no_leading_system_run() {
        assert_eq!(leading_system_run(&[]), 0);
    }

    #[test]
    fn a_wholly_system_history_is_entirely_the_run() {
        let messages = vec![Message::system("a"), Message::system("b")];
        assert_eq!(leading_system_run(&messages), 2);
    }

    #[test]
    fn no_leading_system_turn_is_a_zero_length_run() {
        let messages = vec![Message::user("hi")];
        assert_eq!(leading_system_run(&messages), 0);
    }

    #[test]
    fn the_run_stops_at_the_first_non_system_turn() {
        let messages = vec![
            Message::system("a"),
            Message::system("b"),
            Message::user("hi"),
            Message::system("mid-conversation"),
        ];
        assert_eq!(leading_system_run(&messages), 2);
    }

    #[test]
    fn it_accepts_a_borrowed_reference_iterator_too() {
        let messages = [Message::system("a"), Message::user("hi")];
        let refs: Vec<&Message> = messages.iter().collect();
        assert_eq!(leading_system_run(refs), 1);
    }
}
