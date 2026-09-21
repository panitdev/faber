//! The one place the harness-facing (camelCase, JS-shaped) vocabulary and
//! `llm`'s (snake_case, Rust-shaped) vocabulary meet — `evaluation.md` item 4.
//!
//! Every type here that crosses the boundary has a test that actually calls
//! a serializer — not a round trip through both directions, since the data
//! flow itself isn't symmetric: events (`LLMEvent` and friends) only ever go
//! *out* to JS, so those are tested `Serialize`-only; requests (`LLMRequest`,
//! `WireTurn`) only ever come *in*, so those are tested `Deserialize`-only.
//! `Message` is the one type used both ways and gets an actual round trip.
//!
//! The reason any of this is tested at all rather than trusted from the
//! derive: `evaluation.md` §D1 found that `llm::Delta`'s original shape was
//! an internally-tagged enum over newtype variants, which *compiles*,
//! derives cleanly, and fails only at `serde_json::to_string` time
//! (`TaggedSerializer::bad_type`, raised before any backend runs). A
//! type-checked mapping proves nothing about whether it can cross the ops
//! boundary; only a test that actually serializes (or deserializes) does.
//!
//! Field names are camelCase; discriminant strings stay snake_case, per
//! `types.d.ts`'s header comment — they're protocol values echoed from
//! providers, not identifiers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use llm::{ContentBlock, Role as LlmRole};

// ---------------------------------------------------------------------------
// Content
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
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
    #[serde(rename_all = "camelCase")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
    Unknown {
        raw: Value,
    },
}

impl From<&ContentBlock> for Content {
    fn from(block: &ContentBlock) -> Self {
        match block {
            ContentBlock::Text { text } => Content::Text { text: text.clone() },
            ContentBlock::Thinking {
                thinking,
                signature,
            } => Content::Thinking {
                thinking: thinking.clone(),
                signature: signature.clone(),
            },
            ContentBlock::ToolUse { id, name, input } => Content::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Content::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                is_error: *is_error,
            },
            ContentBlock::Unknown { raw } => Content::Unknown { raw: raw.clone() },
        }
    }
}

impl From<Content> for ContentBlock {
    fn from(content: Content) -> Self {
        match content {
            Content::Text { text } => ContentBlock::Text { text },
            Content::Thinking {
                thinking,
                signature,
            } => ContentBlock::Thinking {
                thinking,
                signature,
            },
            Content::ToolUse { id, name, input } => ContentBlock::ToolUse { id, name, input },
            Content::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            },
            Content::Unknown { raw } => ContentBlock::Unknown { raw },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl From<LlmRole> for Role {
    fn from(role: LlmRole) -> Self {
        match role {
            LlmRole::System => Role::System,
            LlmRole::User => Role::User,
            LlmRole::Assistant => Role::Assistant,
        }
    }
}

impl From<Role> for LlmRole {
    fn from(role: Role) -> Self {
        match role {
            Role::System => LlmRole::System,
            Role::User => LlmRole::User,
            Role::Assistant => LlmRole::Assistant,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Present iff this message belongs to the committed lineage — absent
    /// for a message a harness is sending by value for the first time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub role: Role,
    pub content: Vec<Content>,
}

impl Message {
    /// A committed message, with the id Core minted for it attached — what
    /// `history.read()` hands back and what one turn of a `Completion`'s
    /// message carries.
    pub fn committed(id: &str, message: &llm::Message) -> Self {
        Message {
            id: Some(id.to_string()),
            role: message.role.into(),
            content: message.content.iter().map(Content::from).collect(),
        }
    }
}

impl From<&llm::Message> for Message {
    fn from(message: &llm::Message) -> Self {
        Message {
            id: None,
            role: message.role.into(),
            content: message.content.iter().map(Content::from).collect(),
        }
    }
}

impl From<Message> for llm::Message {
    fn from(message: Message) -> Self {
        llm::Message {
            role: message.role.into(),
            content: message
                .content
                .into_iter()
                .map(ContentBlock::from)
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// A turn on the wire: a full message (possibly id-bearing), or a bare
// reference to one Core already holds.
// ---------------------------------------------------------------------------

/// `{ readonly id: string }` on the JS side — a reference to a message Core
/// already holds, with no body. `#[serde(deny_unknown_fields)]` is what makes
/// this safe to try *after* `Message` under `#[serde(untagged)]`: without it,
/// untagged's default ignore-unknown-fields behaviour would let a bare
/// `{id: "..."}` also match here even when `Message` was tried first and
/// failed only because the input carried extra fields it didn't recognise.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageRef {
    pub id: String,
}

/// `Turn = Message | MessageRef` in `types.d.ts`. `#[serde(untagged)]` is
/// safe here — unlike D1, this is a *deserialize*-only concern (neither
/// variant needs to serialize as a `Turn`) and untagged deserialization is
/// ordinary supported serde, not the tagged-enum path §D1 hit.
///
/// `Message` is tried first. An id-bearing `Message` (`{id, role, content}`)
/// can never be swallowed by `MessageRef` even though `MessageRef` only
/// needs `id` to match, because `Message` is checked first and succeeds; a
/// bare `{id}` fails `Message` (missing `role`/`content`) and falls through
/// to `MessageRef`. Both guards — the ordering and `deny_unknown_fields` —
/// are load-bearing together; if this ordering were ever flipped,
/// `deny_unknown_fields` alone would still stop `MessageRef` from silently
/// swallowing a full `Message`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WireTurn {
    Message(Message),
    Ref(MessageRef),
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl From<&llm::ToolDef> for ToolDef {
    fn from(tool: &llm::ToolDef) -> Self {
        ToolDef {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        }
    }
}

impl From<ToolDef> for llm::ToolDef {
    fn from(tool: ToolDef) -> Self {
        llm::ToolDef {
            name: tool.name,
            description: tool.description,
            input_schema: tool.input_schema,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: String,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

// ---------------------------------------------------------------------------
// Reasoning and sampling
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Thinking {
    Adaptive { display: ThinkingDisplay },
    Disabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingDisplay {
    Omitted,
    Summarized,
}

impl From<Thinking> for llm::Thinking {
    fn from(thinking: Thinking) -> Self {
        match thinking {
            Thinking::Adaptive { display } => llm::Thinking::Adaptive {
                display: match display {
                    ThinkingDisplay::Omitted => llm::ThinkingDisplay::Omitted,
                    ThinkingDisplay::Summarized => llm::ThinkingDisplay::Summarized,
                },
            },
            Thinking::Disabled => llm::Thinking::Disabled,
        }
    }
}

impl From<llm::Thinking> for Thinking {
    fn from(thinking: llm::Thinking) -> Self {
        match thinking {
            llm::Thinking::Adaptive { display } => Thinking::Adaptive {
                display: match display {
                    llm::ThinkingDisplay::Omitted => ThinkingDisplay::Omitted,
                    llm::ThinkingDisplay::Summarized => ThinkingDisplay::Summarized,
                },
            },
            llm::Thinking::Disabled => Thinking::Disabled,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl From<Effort> for llm::Effort {
    fn from(effort: Effort) -> Self {
        match effort {
            Effort::Low => llm::Effort::Low,
            Effort::Medium => llm::Effort::Medium,
            Effort::High => llm::Effort::High,
            Effort::XHigh => llm::Effort::XHigh,
            Effort::Max => llm::Effort::Max,
        }
    }
}

impl From<llm::Effort> for Effort {
    fn from(effort: llm::Effort) -> Self {
        match effort {
            llm::Effort::Low => Effort::Low,
            llm::Effort::Medium => Effort::Medium,
            llm::Effort::High => Effort::High,
            llm::Effort::XHigh => Effort::XHigh,
            llm::Effort::Max => Effort::Max,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sampling {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(rename = "topP", skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(rename = "topK", skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
}

impl From<llm::Sampling> for Sampling {
    fn from(sampling: llm::Sampling) -> Self {
        Sampling {
            temperature: sampling.temperature,
            top_p: sampling.top_p,
            top_k: sampling.top_k,
        }
    }
}

impl From<Sampling> for llm::Sampling {
    fn from(sampling: Sampling) -> Self {
        llm::Sampling {
            temperature: sampling.temperature,
            top_p: sampling.top_p,
            top_k: sampling.top_k,
        }
    }
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    None,
    Tool { name: String },
}

impl From<ToolChoice> for llm::ToolChoice {
    fn from(choice: ToolChoice) -> Self {
        match choice {
            ToolChoice::Auto => llm::ToolChoice::Auto,
            ToolChoice::Any => llm::ToolChoice::Any,
            ToolChoice::None => llm::ToolChoice::None,
            ToolChoice::Tool { name } => llm::ToolChoice::Tool { name },
        }
    }
}

impl From<llm::ToolChoice> for ToolChoice {
    fn from(choice: llm::ToolChoice) -> Self {
        match choice {
            llm::ToolChoice::Auto => ToolChoice::Auto,
            llm::ToolChoice::Any => ToolChoice::Any,
            llm::ToolChoice::None => ToolChoice::None,
            llm::ToolChoice::Tool { name } => ToolChoice::Tool { name },
        }
    }
}

/// The harness-facing request. No `model`: roles are resolved above the
/// harness (`abstract.md` §4), and this crate's caller binds one client (and
/// therefore one model) per run — see [`crate::state::HarnessState`].
///
/// Every field but `messages` defaults to the committed lineage's own
/// baseline (`proposal.md` §4) — that merge happens in
/// `crate::ops::op_llm_stream_open`, not here; this type only has to make
/// "unset" and "explicitly cleared" distinguishable, which is why `sampling`
/// and `stop_sequences` are `Option` rather than defaulted to empty the way
/// they used to be.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LLMRequest {
    pub messages: Vec<WireTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub thinking: Option<Thinking>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub sampling: Option<Sampling>,
    #[serde(default)]
    pub stop_sequences: Option<Vec<String>>,
    /// Merged into the request body verbatim, for provider features this
    /// contract has no opinion about yet. Colliding keys are ignored.
    #[serde(default)]
    pub extra: Option<serde_json::Map<String, Value>>,
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockStart {
    Text,
    Thinking,
    ToolUse { id: String, name: String },
    Unknown { raw: Value },
}

impl From<llm::BlockStart> for BlockStart {
    fn from(start: llm::BlockStart) -> Self {
        match start {
            llm::BlockStart::Text => BlockStart::Text,
            llm::BlockStart::Thinking => BlockStart::Thinking,
            llm::BlockStart::ToolUse { id, name } => BlockStart::ToolUse { id, name },
            llm::BlockStart::Unknown { raw } => BlockStart::Unknown { raw },
        }
    }
}

/// Struct variants throughout — never a newtype variant carrying a single
/// value. That shape is exactly what §D1 found broken: an internally-tagged
/// enum cannot serialize a newtype variant, and `serde_v8` hits the identical
/// `TaggedSerializer::bad_type` panic path `serde_json` does.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ThinkingSignature {
        signature: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolInputJson {
        partial_json: String,
    },
    Unknown {
        raw: Value,
    },
}

impl From<llm::Delta> for Delta {
    fn from(delta: llm::Delta) -> Self {
        match delta {
            llm::Delta::Text { content } => Delta::Text { text: content },
            llm::Delta::Thinking { content } => Delta::Thinking { thinking: content },
            llm::Delta::ThinkingSignature { content } => {
                Delta::ThinkingSignature { signature: content }
            }
            llm::Delta::ToolInputJson { content } => Delta::ToolInputJson {
                partial_json: content,
            },
            llm::Delta::Unknown { raw } => Delta::Unknown { raw },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    ContextWindowExceeded,
    Other { content: String },
}

impl From<llm::StopReason> for StopReason {
    fn from(reason: llm::StopReason) -> Self {
        match reason {
            llm::StopReason::EndTurn => StopReason::EndTurn,
            llm::StopReason::MaxTokens => StopReason::MaxTokens,
            llm::StopReason::StopSequence => StopReason::StopSequence,
            llm::StopReason::ToolUse => StopReason::ToolUse,
            llm::StopReason::PauseTurn => StopReason::PauseTurn,
            llm::StopReason::Refusal => StopReason::Refusal,
            llm::StopReason::ContextWindowExceeded => StopReason::ContextWindowExceeded,
            llm::StopReason::Other(content) => StopReason::Other { content },
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StopDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

impl From<llm::StopDetails> for StopDetails {
    fn from(details: llm::StopDetails) -> Self {
        StopDetails {
            category: details.category,
            explanation: details.explanation,
        }
    }
}

/// One provider report. Absent fields are omitted, never sent as `null` —
/// `evaluation.md` §D5: an absent count and a reported zero are different
/// facts, and a `null` would collapse that distinction on the wire.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(rename = "cacheReadTokens", skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(rename = "cacheWriteTokens", skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// What the provider says the call cost, when it reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl From<llm::UsageDelta> for UsageDelta {
    fn from(usage: llm::UsageDelta) -> Self {
        UsageDelta {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cost: usage.cost,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LLMEvent {
    MessageStart {
        id: String,
        model: String,
        usage: UsageDelta,
    },
    BlockStart {
        index: usize,
        block: BlockStart,
    },
    BlockDelta {
        index: usize,
        delta: Delta,
    },
    BlockStop {
        index: usize,
    },
    #[serde(rename_all = "camelCase")]
    MessageDelta {
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_reason: Option<StopReason>,
        #[serde(skip_serializing_if = "Option::is_none")]
        stop_details: Option<StopDetails>,
        usage: UsageDelta,
    },
    MessageStop,
    Unknown {
        raw: Value,
    },
}

impl From<llm::Event> for LLMEvent {
    fn from(event: llm::Event) -> Self {
        match event {
            llm::Event::MessageStart { id, model, usage } => LLMEvent::MessageStart {
                id,
                model,
                usage: usage.into(),
            },
            llm::Event::BlockStart { index, block } => LLMEvent::BlockStart {
                index,
                block: block.into(),
            },
            llm::Event::BlockDelta { index, delta } => LLMEvent::BlockDelta {
                index,
                delta: delta.into(),
            },
            llm::Event::BlockStop { index } => LLMEvent::BlockStop { index },
            llm::Event::MessageDelta {
                stop_reason,
                stop_details,
                usage,
            } => LLMEvent::MessageDelta {
                stop_reason: stop_reason.map(StopReason::from),
                stop_details: stop_details.map(StopDetails::from),
                usage: usage.into(),
            },
            llm::Event::MessageStop => LLMEvent::MessageStop,
            llm::Event::Unknown { raw } => LLMEvent::Unknown { raw },
        }
    }
}

// ---------------------------------------------------------------------------
// Completion, and the request-options surface `commit`/`committedRequest`
// deal in — none of this existed under the span model, since a `Call` had
// nothing to return but a `Span`.
// ---------------------------------------------------------------------------

/// An accumulated total, as `Completion.usage` carries it — distinct from
/// `UsageDelta`: every field here really was reported, not merely not-yet.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheWriteTokens")]
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    /// Provider-reported cost, omitted when the provider reported none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl From<&llm::Usage> for Usage {
    fn from(usage: &llm::Usage) -> Self {
        Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cost: usage.cost,
        }
    }
}

/// What `Call.completion` resolves to. `Serialize`-only — like `LLMEvent`,
/// this only ever crosses the boundary outward.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub id: String,
    pub model: String,
    pub message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// Populated only when `stop_reason` is `refusal`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_details: Option<StopDetails>,
    pub usage: Usage,
}

impl From<&llm::Completion> for Completion {
    fn from(completion: &llm::Completion) -> Self {
        Completion {
            id: completion.id.clone(),
            model: completion.model.clone(),
            message: Message::from(&completion.message),
            stop_reason: completion.stop_reason.clone().map(StopReason::from),
            stop_details: completion.stop_details.clone().map(StopDetails::from),
            usage: Usage::from(&completion.usage),
        }
    }
}

/// `Omit<LLMRequest, "messages">` — every option field, frozen as the
/// committed lineage was built with it. What `Context.committedRequest()`
/// returns, and what `op_llm_stream_open`'s merge fills unset request fields
/// from (`proposal.md` §4).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    pub tools: Vec<ToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<Sampling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Map<String, Value>>,
}

impl From<&crate::state::Baseline> for RequestOptions {
    fn from(baseline: &crate::state::Baseline) -> Self {
        RequestOptions {
            max_tokens: baseline.max_tokens,
            tools: baseline.tools.iter().map(ToolDef::from).collect(),
            tool_choice: baseline.tool_choice.clone().map(ToolChoice::from),
            thinking: baseline.thinking.map(Thinking::from),
            effort: baseline.effort.map(Effort::from),
            sampling: baseline.sampling.map(Sampling::from),
            stop_sequences: baseline.stop_sequences.clone(),
            extra: baseline.extra.clone(),
        }
    }
}

/// `commit(call, options?)`'s second argument. `partial` opts into adopting
/// a call that ended with a truncated completion (`proposal.md` §6.3) —
/// silently committing a truncation poisons every subsequent turn, so the
/// default is to refuse.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitOptions {
    #[serde(default)]
    pub partial: bool,
}

// ---------------------------------------------------------------------------
// The transcript vocabulary: what a harness may yield from `execute`.
// ---------------------------------------------------------------------------

/// `LLMEvent`, plus exactly one harness-authored addition: a tool result. A
/// harness that dispatches a tool otherwise has no way to represent what it
/// did without faking a text block (H9.2 in `history-abstract.md`). H9.2
/// stays open beyond that — this is deliberately not a wider presentation
/// vocabulary.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum TranscriptEvent {
    Model(LLMEvent),
    #[serde(rename_all = "camelCase")]
    ToolResult {
        #[serde(rename = "type")]
        kind: ToolResultTag,
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
pub enum ToolResultTag {
    #[serde(rename = "tool_result")]
    ToolResult,
}

// ---------------------------------------------------------------------------
// Failure
// ---------------------------------------------------------------------------

/// Mirrors `types.d.ts`'s `HarnessError`. Not `Serialize`/`Deserialize` on
/// the JS side directly — `crate::error::OpError` carries these fields as
/// individual JS `Error` *properties* (`types.d.ts:296`'s requirement that a
/// harness not have to regex an error message to decide whether to retry).
/// `Serialize` for the *frame log*, not for the isolate: `crates/api` writes
/// this into `exchange.outcome`, which is Core's own record of what happened.
/// The harness-facing path is still `OpError`'s individual JS properties.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessErrorInfo {
    pub kind: &'static str,
    pub message: String,
    pub transient: bool,
    pub status: Option<u16>,
    pub request_id: Option<String>,
}

impl HarnessErrorInfo {
    /// A stop asked for from outside the run ([`crate::interrupt`]).
    ///
    /// `cancelled` is `types.d.ts`'s own kind for this, and it is deliberately
    /// one of the ordinary failure kinds rather than a channel of its own —
    /// "so a harness cannot handle one and silently ignore the other". Not
    /// transient: retrying is exactly what was just asked not to happen.
    pub fn cancelled() -> Self {
        HarnessErrorInfo {
            kind: "cancelled",
            message: "the run was interrupted".to_owned(),
            transient: false,
            status: None,
            request_id: None,
        }
    }
}

impl From<&llm::Error> for HarnessErrorInfo {
    fn from(error: &llm::Error) -> Self {
        let transient = error.is_transient();
        match error {
            llm::Error::Transport(_) => HarnessErrorInfo {
                kind: "transport",
                message: error.to_string(),
                transient,
                status: None,
                request_id: None,
            },
            llm::Error::Api {
                status, request_id, ..
            } => HarnessErrorInfo {
                kind: "api",
                message: error.to_string(),
                transient,
                status: *status,
                request_id: request_id.clone(),
            },
            llm::Error::Decode(_) => HarnessErrorInfo {
                kind: "decode",
                message: error.to_string(),
                transient,
                status: None,
                request_id: None,
            },
            llm::Error::EmptyResponse => HarnessErrorInfo {
                kind: "empty_response",
                message: error.to_string(),
                transient,
                status: None,
                request_id: None,
            },
            llm::Error::ToolInput { .. } => HarnessErrorInfo {
                kind: "tool_input",
                message: error.to_string(),
                transient,
                status: None,
                request_id: None,
            },
            llm::Error::ExpectedUserTurn => HarnessErrorInfo {
                kind: "expected_user_turn",
                message: error.to_string(),
                transient,
                status: None,
                request_id: None,
            },
            // The same kind as `SpanScope`: a span rendered under another
            // reasoning policy is out of scope for the same reason and asks
            // the same thing of a harness. The message names which part drifted.
            llm::Error::SpanScope { .. } | llm::Error::SpanReasoning { .. } => HarnessErrorInfo {
                kind: "span_scope",
                message: error.to_string(),
                transient: false,
                status: None,
                request_id: None,
            },
            llm::Error::SpanPosition { .. } => HarnessErrorInfo {
                kind: "span_position",
                message: error.to_string(),
                transient: false,
                status: None,
                request_id: None,
            },
        }
    }
}

impl From<&crate::validate::Refusal> for HarnessErrorInfo {
    fn from(refusal: &crate::validate::Refusal) -> Self {
        HarnessErrorInfo {
            kind: refusal.kind(),
            message: refusal.message(),
            // Refusals are local, structural facts about the request — never
            // a provider's classification of transience, so there is nothing
            // for this to mirror the way `llm::Error::is_transient` does.
            transient: false,
            status: None,
            request_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test §D1 says matters: not "does this typecheck" but "does this
    /// actually serialize" — every variant, through a real serializer.
    #[test]
    fn every_delta_variant_serializes() {
        let deltas = [
            Delta::Text { text: "hi".into() },
            Delta::Thinking {
                thinking: "hmm".into(),
            },
            Delta::ThinkingSignature {
                signature: "sig".into(),
            },
            Delta::ToolInputJson {
                partial_json: "{}".into(),
            },
            Delta::Unknown { raw: Value::Null },
        ];
        for delta in deltas {
            let json = serde_json::to_value(&delta).expect("Delta must serialize");
            assert!(json["type"].is_string());
        }
    }

    #[test]
    fn tool_input_json_delta_uses_partial_json() {
        let delta = Delta::ToolInputJson {
            partial_json: "{\"a\":1".into(),
        };
        let json = serde_json::to_value(&delta).unwrap();
        assert_eq!(json["type"], "tool_input_json");
        assert_eq!(json["partialJson"], "{\"a\":1");
    }

    #[test]
    fn stop_reason_other_round_trips_through_a_real_serializer() {
        let reason = StopReason::Other {
            content: "x".into(),
        };
        let json = serde_json::to_value(&reason).unwrap();
        assert_eq!(json, serde_json::json!({"type": "other", "content": "x"}));
    }

    #[test]
    fn usage_delta_omits_absent_fields_rather_than_nulling_them() {
        let usage = UsageDelta {
            input_tokens: Some(10),
            ..Default::default()
        };
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json, serde_json::json!({"inputTokens": 10}));
    }

    #[test]
    fn an_upstream_cost_crosses_the_boundary_as_itself() {
        let usage = UsageDelta::from(llm::UsageDelta {
            cost: Some(0.95),
            ..Default::default()
        });
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json, serde_json::json!({"cost": 0.95}));
    }

    #[test]
    fn reasoning_tokens_maps_through_from_llm() {
        let usage = UsageDelta::from(llm::UsageDelta {
            reasoning_tokens: Some(7),
            ..Default::default()
        });
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json, serde_json::json!({"reasoningTokens": 7}));
    }

    #[test]
    fn a_message_round_trips_plural_content() {
        let message = llm::Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "reasoning".into(),
                signature: Some("sig".into()),
            },
            ContentBlock::Text {
                text: "answer".into(),
            },
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "read".into(),
                input: serde_json::json!({}),
            },
        ]);
        let wire = Message::from(&message);
        let json = serde_json::to_value(&wire).unwrap();
        let back: Message = serde_json::from_value(json).unwrap();
        let round_tripped: llm::Message = back.into();
        assert_eq!(round_tripped.content.len(), 3);
    }

    #[test]
    fn a_ref_turn_deserializes_untagged() {
        let json = serde_json::json!({"id": "m1"});
        let turn: WireTurn = serde_json::from_value(json).unwrap();
        assert!(matches!(turn, WireTurn::Ref(MessageRef { id }) if id == "m1"));
    }

    #[test]
    fn an_id_bearing_message_is_not_swallowed_by_message_ref() {
        let json = serde_json::json!({
            "id": "m1",
            "role": "user",
            "content": [{"type": "text", "text": "hi"}],
        });
        let turn: WireTurn = serde_json::from_value(json).unwrap();
        assert!(matches!(turn, WireTurn::Message(Message { id: Some(id), .. }) if id == "m1"));
    }

    #[test]
    fn a_message_turn_deserializes_untagged() {
        let json = serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]});
        let turn: WireTurn = serde_json::from_value(json).unwrap();
        assert!(matches!(turn, WireTurn::Message(_)));
    }

    #[test]
    fn tool_result_transcript_event_serializes_with_a_tag() {
        let event = TranscriptEvent::ToolResult {
            kind: ToolResultTag::ToolResult,
            tool_use_id: "t1".into(),
            content: "42".into(),
            is_error: false,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["toolUseId"], "t1");
    }

    #[test]
    fn a_model_transcript_event_serializes_as_its_llm_event() {
        let event = TranscriptEvent::Model(LLMEvent::MessageStop);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "message_stop");
    }
}
