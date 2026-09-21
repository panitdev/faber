//! Translation between the neutral types and the OpenAI Chat Completions API.
//!
//! Hand-written rather than derived: the neutral types' own `serde` impls are
//! for persistence, and a provider's wire format is free to drift away from
//! them.

use std::collections::{BTreeSet, HashMap};

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

use crate::error::Error;
use crate::event::{BlockStart, Delta, Event};
use crate::span::{RenderedRequest, RenderedSpan, append_element, split_span, wrap_array};
use crate::types::{
    ContentBlock, Effort, Message, ReasoningHistory, Request, Role, StopReason, Thinking,
    ToolChoice, UsageDelta,
};

const PROVIDER: &str = "openai";

/// What this wire does when a request expresses no preference.
///
/// Chat Completions has no reasoning field of its own — `reasoning_content` is
/// a convention several providers grew independently, and OpenAI's own
/// endpoint rejects it on an assistant turn. Dropping is the only default that
/// is safe against every endpoint speaking this wire; a provider that wants
/// its reasoning back says so per model.
pub const DEFAULT_REASONING: ReasoningHistory = ReasoningHistory::Omitted;

/// Renders a [`Request`] to the Chat Completions wire format.
///
/// Unlike Anthropic there is one region, `messages` — a system turn stays
/// inline here regardless of position, so nothing is ever hoisted out of it.
pub(super) fn render(request: &Request) -> Result<RenderedRequest, Error> {
    let reasoning = request.reasoning_history.unwrap_or(DEFAULT_REASONING);
    let (span, new_turns) = split_span(&request.messages, PROVIDER, &request.model, reasoning)?;

    let mut messages_bytes = span
        .and_then(|span| span.regions.get("messages"))
        .cloned()
        .unwrap_or_default();

    for message in new_turns {
        match message.role {
            Role::System => append_element(
                &mut messages_bytes,
                &json!({"role": "system", "content": message.text()}),
            ),
            Role::User => append_user_elements(message, &mut messages_bytes),
            Role::Assistant => {
                append_element(&mut messages_bytes, &assistant_message(message, reasoning))
            }
        }
    }

    let prefix = RenderedSpan {
        provider: PROVIDER.into(),
        model: request.model.clone(),
        reasoning,
        regions: [("messages".to_string(), messages_bytes.clone())]
            .into_iter()
            .collect(),
    };

    let body = build_body(request, &messages_bytes)?;
    Ok(RenderedRequest { body, prefix })
}

fn build_body(request: &Request, messages_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut head = request_head(request);
    head.remove("messages");

    let messages_raw = RawValue::from_string(wrap_array(messages_bytes)?)
        .map_err(|error| Error::Decode(format!("rendered messages region: {error}")))?;

    #[derive(Serialize)]
    struct Body<'a> {
        #[serde(flatten)]
        head: Map<String, Value>,
        messages: &'a RawValue,
    }

    serde_json::to_vec(&Body {
        head,
        messages: &messages_raw,
    })
    .map_err(|error| Error::Decode(format!("failed to serialize request body: {error}")))
}

/// Every field except `messages`, which the caller splices in separately so
/// its bytes never round-trip through this map.
fn request_head(request: &Request) -> Map<String, Value> {
    let mut body = Map::new();

    // Caller-supplied passthrough first, so modelled fields always win.
    for (key, value) in &request.extra {
        body.insert(key.clone(), value.clone());
    }

    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    // Token accounting arrives in a usage-only chunk just before [DONE].
    body.insert("stream_options".into(), json!({"include_usage": true}));
    body.insert("max_completion_tokens".into(), json!(request.max_tokens));

    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": tool.name,
                                "description": tool.description,
                                "parameters": tool.input_schema,
                            },
                        })
                    })
                    .collect(),
            ),
        );
    }
    if let Some(choice) = &request.tool_choice {
        body.insert(
            "tool_choice".into(),
            match choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::Any => json!("required"),
                ToolChoice::None => json!("none"),
                ToolChoice::Tool { name } => {
                    json!({"type": "function", "function": {"name": name}})
                }
            },
        );
    }
    if let Some(Thinking::Disabled) = request.thinking {
        // Reasoning is one knob here. Adaptive leaves it at the provider's
        // default; Chat Completions exposes no summary control.
        body.insert("reasoning_effort".into(), json!("none"));
    }
    if let Some(effort) = request.effort {
        // Effort shares the knob and is the more specific setting, so it
        // wins. OpenAI tops out at xhigh — Max is clamped, not rejected.
        body.insert(
            "reasoning_effort".into(),
            json!(match effort {
                Effort::Low => "low",
                Effort::Medium => "medium",
                Effort::High => "high",
                Effort::XHigh | Effort::Max => "xhigh",
            }),
        );
    }
    // Omitted when unset, mirroring the Anthropic side: an explicit default
    // is not the same as an absent field. `top_k` has no OpenAI form.
    if let Some(temperature) = request.sampling.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = request.sampling.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if !request.stop_sequences.is_empty() {
        body.insert("stop".into(), json!(request.stop_sequences));
    }

    body
}

/// A user turn can hold both text and tool results; on this wire those are
/// different roles, so one neutral message can become several — each
/// appended as its own element, in order, straight into the byte region.
fn append_user_elements(message: &Message, region: &mut Vec<u8>) {
    let mut parts = Vec::new();
    let mut results = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => parts.push(json!({"type": "text", "text": text})),
            // A tool result is its own message, not a content part. `is_error`
            // has no wire equivalent; the content carries it.
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => results.push(json!({
                "role": "tool",
                "tool_call_id": tool_use_id,
                "content": content,
            })),
            // Tool use in a user turn, thinking, and unmodelled blocks have
            // no user-role form; they are dropped rather than mangled.
            _ => {}
        }
    }

    // Results first, whatever order the blocks came in: a `tool` message is
    // only accepted immediately after the assistant turn that asked for it, so
    // text sharing the turn has to follow rather than split the run.
    for result in &results {
        append_element(region, result);
    }
    if !parts.is_empty() {
        append_element(region, &json!({"role": "user", "content": parts}));
    }
}

fn assistant_message(message: &Message, reasoning: ReasoningHistory) -> Value {
    let mut parts = Vec::new();
    let mut calls = Vec::new();
    let mut thinking = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text } => parts.push(json!({"type": "text", "text": text})),
            ContentBlock::ToolUse { id, name, input } => calls.push(json!({
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": input.to_string()},
            })),
            // Sent back only when the model asked for it. There is no
            // signature field on this wire, so `Text` and `Full` render the
            // same bytes — the signature has nowhere to go.
            ContentBlock::Thinking { thinking: text, .. }
                if reasoning != ReasoningHistory::Omitted =>
            {
                thinking.push_str(text);
            }
            // Unknown blocks belong to another provider's dialect.
            _ => {}
        }
    }

    let mut value = json!({"role": "assistant"});
    // Concatenated rather than one field per block: `reasoning_content` is a
    // single string on every provider that reads it, so a turn that reasoned
    // twice around a tool call has one place to land.
    if !thinking.is_empty() {
        value["reasoning_content"] = json!(thinking);
    }
    // Content may be null only when tool calls carry the turn. With neither —
    // a turn whose only block was thinking, dropped just above — null would be
    // rejected, so the turn goes back as empty text. Reasoning does not count
    // as content here: a provider that ignores `reasoning_content` would be
    // handed a turn with nothing in it.
    value["content"] = match (parts.is_empty(), calls.is_empty()) {
        (true, true) => json!(""),
        (true, false) => Value::Null,
        (false, _) => Value::Array(parts),
    };
    if !calls.is_empty() {
        value["tool_calls"] = Value::Array(calls);
    }
    value
}

/// Turns `chat.completion.chunk` frames into neutral events.
///
/// Chat Completions streams flat deltas, so block boundaries are synthesized
/// here: a block opens when its first delta arrives and stays open until the
/// turn ends.
///
/// Several blocks may be open at once. That is deliberate — a single delta can
/// carry both `content` and `tool_calls`, and argument fragments for one call
/// can arrive on either side of a run of text. Closing a block as soon as a
/// different kind of content appeared would strand those later fragments in a
/// block that had already stopped, so the wire indices keep their own blocks
/// and everything closes together at the end of the turn.
#[derive(Default)]
pub(super) struct StreamDecoder {
    started: bool,
    text: Option<usize>,
    thinking: Option<usize>,
    /// Wire tool-call index → neutral block index.
    tool_calls: HashMap<usize, usize>,
    /// Blocks opened and not yet stopped, in ascending index order.
    open: BTreeSet<usize>,
    next_index: usize,
}

impl StreamDecoder {
    pub(super) fn push_chunk(&mut self, chunk: &Value) -> Result<Vec<Event>, Error> {
        // Some compatible endpoints deliver mid-stream failures as frames.
        // Others stamp a null `error` on every healthy chunk, so an absent
        // failure and a present-but-null one have to read the same.
        if let Some(error) = chunk.get("error").filter(|value| !value.is_null()) {
            return Err(Error::Api {
                status: None,
                kind: error.get("type").and_then(Value::as_str).map(str::to_owned),
                // A bare string is the other shape seen in the wild.
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .or_else(|| error.as_str())
                    .unwrap_or("stream reported an error")
                    .to_owned(),
                request_id: None,
            });
        }

        let mut events = Vec::new();

        if !self.started {
            self.started = true;
            events.push(Event::MessageStart {
                id: string_at(chunk, "id"),
                model: string_at(chunk, "model"),
                usage: usage_from(chunk.get("usage")),
            });
        }

        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first());

        let mut stop_reason = None;
        if let Some(choice) = choice {
            if let Some(delta) = choice.get("delta") {
                self.push_delta(delta, &mut events);
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                stop_reason = Some(stop_reason_from(reason));
            }
        }

        let usage = usage_from(chunk.get("usage"));
        if stop_reason.is_some() {
            self.close_blocks(&mut events);
            events.push(Event::MessageDelta {
                stop_reason,
                stop_details: None,
                usage,
            });
        } else if usage != UsageDelta::default() {
            // The usage-only chunk that closes the stream.
            events.push(Event::MessageDelta {
                stop_reason: None,
                stop_details: None,
                usage,
            });
        }

        Ok(events)
    }

    fn push_delta(&mut self, delta: &Value, events: &mut Vec<Event>) {
        // Reasoning arrives under `reasoning_content` (`reasoning` on some
        // compatible endpoints); no signature ever closes it.
        let thinking = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str);
        if let Some(thinking) = thinking.filter(|text| !text.is_empty()) {
            let index = match self.thinking {
                Some(index) => index,
                None => {
                    let index = self.open_block(BlockStart::Thinking, events);
                    self.thinking = Some(index);
                    // Text that resumes after this reasoning belongs after it,
                    // not folded back into the run that preceded it.
                    self.text = None;
                    index
                }
            };
            events.push(Event::BlockDelta {
                index,
                delta: Delta::Thinking {
                    content: thinking.to_owned(),
                },
            });
        }

        // A refusal is streamed content on this API; it stays readable text,
        // and the finish reason carries that a refusal happened.
        let text = delta.get("content").and_then(Value::as_str);
        let refusal = delta.get("refusal").and_then(Value::as_str);
        for text in [text, refusal]
            .into_iter()
            .flatten()
            .filter(|text| !text.is_empty())
        {
            let index = match self.text {
                Some(index) => index,
                None => {
                    let index = self.open_block(BlockStart::Text, events);
                    self.text = Some(index);
                    self.thinking = None;
                    index
                }
            };
            events.push(Event::BlockDelta {
                index,
                delta: Delta::Text {
                    content: text.to_owned(),
                },
            });
        }

        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                self.push_tool_call(call, events);
            }
        }
    }

    fn push_tool_call(&mut self, call: &Value, events: &mut Vec<Event>) {
        let wire_index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;

        // Only a call's first fragment carries id and name.
        let index = match self.tool_calls.get(&wire_index) {
            Some(&index) => index,
            None => {
                let name = call
                    .get("function")
                    .map(|function| string_at(function, "name"))
                    .unwrap_or_default();
                self.open_block(
                    BlockStart::ToolUse {
                        id: string_at(call, "id"),
                        name,
                    },
                    events,
                )
            }
        };
        self.tool_calls.insert(wire_index, index);

        if let Some(arguments) = call
            .get("function")
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .filter(|arguments| !arguments.is_empty())
        {
            events.push(Event::BlockDelta {
                index,
                delta: Delta::ToolInputJson {
                    content: arguments.to_owned(),
                },
            });
        }
    }

    /// Opens a new block and leaves it open.
    fn open_block(&mut self, start: BlockStart, events: &mut Vec<Event>) -> usize {
        let index = self.next_index;
        self.next_index += 1;
        self.open.insert(index);
        events.push(Event::BlockStart {
            index,
            block: start,
        });
        index
    }

    /// Closes every open block, in the order they were opened.
    fn close_blocks(&mut self, events: &mut Vec<Event>) {
        for index in std::mem::take(&mut self.open) {
            events.push(Event::BlockStop { index });
        }
        self.text = None;
        self.thinking = None;
        self.tool_calls.clear();
    }

    /// Closes out the turn once the wire is done.
    pub(super) fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        if !self.started {
            return events;
        }
        self.close_blocks(&mut events);
        events.push(Event::MessageStop);
        events
    }
}

fn stop_reason_from(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::EndTurn,
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        other => StopReason::Other(other.to_owned()),
    }
}

/// Reads only the fields the report actually carries: an absent count and a
/// reported zero mean different things.
///
/// `prompt_tokens` includes cache hits, so the neutral input count runs
/// higher than Anthropic's when caching kicks in; the fields are recorded as
/// reported, not normalized.
fn usage_from(usage: Option<&Value>) -> UsageDelta {
    let Some(usage) = usage else {
        return UsageDelta::default();
    };
    let count = |key: &str| usage.get(key).and_then(Value::as_u64);
    let detail = |key: &str| {
        usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get(key))
            .and_then(Value::as_u64)
    };
    UsageDelta {
        input_tokens: count("prompt_tokens"),
        output_tokens: count("completion_tokens"),
        cache_read_input_tokens: detail("cached_tokens"),
        // The first-party API does not report cache writes; aggregators
        // fronting it (OpenRouter) do, under the same details object.
        cache_creation_input_tokens: detail("cache_write_tokens"),
        reasoning_tokens: usage
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64),
        // OpenRouter and similar return what the call cost. Absent on the
        // first-party API, where a configured price is the only answer.
        cost: usage.get("cost").and_then(Value::as_f64),
    }
}

fn string_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Sampling, Turn};

    fn request() -> Request {
        Request::new("gpt-5", vec![Message::user("hi")])
    }

    /// Test-only convenience: renders and parses the body back into a
    /// `Value` for structural assertions. Production code never does this —
    /// see the `*_prefix_bytes_survive_a_round_trip` test below for the
    /// assertion that actually matters (H5).
    fn request_body(request: &Request) -> Value {
        let rendered = render(request).unwrap();
        serde_json::from_slice(&rendered.body).unwrap()
    }

    fn push(request: &mut Request, message: Message) {
        request.messages.push(Turn::Value(message));
    }

    fn insert_leading(request: &mut Request, message: Message) {
        request.messages.insert(0, Turn::Value(message));
    }

    #[test]
    fn bare_request_sends_nothing_it_was_not_given() {
        let body = request_body(&request());
        let object = body.as_object().unwrap();
        assert_eq!(object["model"], json!("gpt-5"));
        assert_eq!(object["stream"], json!(true));
        assert_eq!(object["stream_options"]["include_usage"], json!(true));
        for absent in [
            "temperature",
            "top_p",
            "top_k",
            "reasoning_effort",
            "tools",
            "tool_choice",
            "stop",
        ] {
            assert!(
                !object.contains_key(absent),
                "unexpected `{absent}` in body"
            );
        }
    }

    #[test]
    fn system_becomes_the_first_message() {
        let mut request = request();
        insert_leading(&mut request, Message::system("be brief"));
        let body = request_body(&request);
        assert_eq!(
            body["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(body["messages"][1]["role"], json!("user"));
    }

    #[test]
    fn a_mid_conversation_system_message_stays_in_place() {
        let mut request = request();
        push(&mut request, Message::system("terse mode"));
        let body = request_body(&request);
        assert_eq!(body["messages"][0]["role"], json!("user"));
        assert_eq!(
            body["messages"][1],
            json!({"role": "system", "content": "terse mode"})
        );
    }

    #[test]
    fn effort_maps_to_reasoning_effort() {
        let mut request = request();
        request.effort = Some(Effort::XHigh);
        let body = request_body(&request);
        assert_eq!(body["reasoning_effort"], json!("xhigh"));
    }

    #[test]
    fn max_effort_clamps_to_the_highest_openai_offers() {
        let mut request = request();
        request.effort = Some(Effort::Max);
        let body = request_body(&request);
        assert_eq!(body["reasoning_effort"], json!("xhigh"));
    }

    #[test]
    fn disabled_thinking_turns_reasoning_off() {
        let mut request = request();
        request.thinking = Some(Thinking::Disabled);
        let body = request_body(&request);
        assert_eq!(body["reasoning_effort"], json!("none"));
    }

    #[test]
    fn effort_wins_over_thinking() {
        let mut request = request();
        request.thinking = Some(Thinking::Disabled);
        request.effort = Some(Effort::Low);
        let body = request_body(&request);
        assert_eq!(body["reasoning_effort"], json!("low"));
    }

    #[test]
    fn sampling_is_sent_only_when_set() {
        let mut request = request();
        request.sampling = Sampling {
            temperature: Some(0.5),
            top_k: Some(40),
            ..Sampling::default()
        };
        let body = request_body(&request);
        assert_eq!(body["temperature"], json!(0.5));
        assert!(body.get("top_p").is_none());
        assert!(body.get("top_k").is_none(), "top_k has no OpenAI form");
    }

    #[test]
    fn tool_results_become_tool_messages() {
        let mut request = request();
        push(
            &mut request,
            Message::assistant(vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: json!({"path": "/tmp/x"}),
            }]),
        );
        push(
            &mut request,
            Message {
                role: Role::User,
                content: vec![ContentBlock::tool_result("call_1", "42")],
            },
        );
        let body = request_body(&request);

        let assistant = &body["messages"][1];
        assert_eq!(assistant["tool_calls"][0]["id"], json!("call_1"));
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            json!(r#"{"path":"/tmp/x"}"#)
        );

        assert_eq!(
            body["messages"][2],
            json!({"role": "tool", "tool_call_id": "call_1", "content": "42"})
        );
    }

    #[test]
    fn text_sharing_a_turn_with_a_result_follows_it() {
        let mut request = request();
        push(
            &mut request,
            Message::assistant(vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: json!({}),
            }]),
        );
        // Blocks in the order that would otherwise split the assistant turn
        // from its tool message.
        push(
            &mut request,
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "and then?".into(),
                    },
                    ContentBlock::tool_result("call_1", "42"),
                ],
            },
        );
        let body = request_body(&request);

        assert_eq!(body["messages"][2]["role"], json!("tool"));
        assert_eq!(body["messages"][3]["role"], json!("user"));
    }

    #[test]
    fn a_thinking_only_turn_does_not_serialize_as_null_content() {
        // Reachable when a reasoning model runs out of tokens before writing
        // anything: null content with no tool calls is rejected outright.
        let message = Message::assistant(vec![ContentBlock::Thinking {
            thinking: "hmm".into(),
            signature: None,
        }]);
        assert_eq!(
            assistant_message(&message, DEFAULT_REASONING)["content"],
            json!("")
        );
    }

    #[test]
    fn reasoning_goes_back_as_reasoning_content_when_the_model_wants_it() {
        let message = Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: Some("sig".into()),
            },
            ContentBlock::text("done"),
        ]);
        let value = assistant_message(&message, ReasoningHistory::Full);

        assert_eq!(value["reasoning_content"], json!("hmm"));
        // No signature field exists on this wire, so `Full` and `Text` agree.
        assert_eq!(
            value,
            assistant_message(&message, ReasoningHistory::Text),
            "a signature has nowhere to go on this wire"
        );
    }

    #[test]
    fn reasoning_is_dropped_by_default() {
        let message = Message::assistant(vec![
            ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: None,
            },
            ContentBlock::text("done"),
        ]);
        let value = assistant_message(&message, DEFAULT_REASONING);

        assert_eq!(value.get("reasoning_content"), None);
        assert_eq!(value["content"][0]["text"], json!("done"));
    }

    #[test]
    fn a_reasoning_only_turn_still_carries_content_when_reasoning_is_sent() {
        // `reasoning_content` is not content: a provider that ignores the
        // field would otherwise be handed a turn with nothing in it.
        let message = Message::assistant(vec![ContentBlock::Thinking {
            thinking: "hmm".into(),
            signature: None,
        }]);
        let value = assistant_message(&message, ReasoningHistory::Full);

        assert_eq!(value["reasoning_content"], json!("hmm"));
        assert_eq!(value["content"], json!(""));
    }

    #[test]
    fn tool_calls_still_carry_a_turn_with_null_content() {
        let message = Message::assistant(vec![ContentBlock::ToolUse {
            id: "call_1".into(),
            name: "read_file".into(),
            input: json!({}),
        }]);
        assert_eq!(
            assistant_message(&message, DEFAULT_REASONING)["content"],
            Value::Null
        );
    }

    #[test]
    fn modelled_fields_win_over_extra() {
        let mut request = request();
        request
            .extra
            .insert("model".into(), json!("something-else"));
        request.extra.insert("service_tier".into(), json!("flex"));
        let body = request_body(&request);
        assert_eq!(body["model"], json!("gpt-5"));
        assert_eq!(body["service_tier"], json!("flex"));
    }

    #[test]
    fn turn_one_has_an_empty_prefix_and_no_stray_comma() {
        let empty_prefix = render(&Request::new("gpt-5", vec![])).unwrap().prefix;
        let mut resumed = Request::new("gpt-5", vec![]);
        resumed.messages = vec![Turn::Span(empty_prefix), Turn::Value(Message::user("hi"))];
        let body = request_body(&resumed);
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}])
        );
    }

    #[test]
    fn a_resumed_prefix_splices_byte_identically() {
        let mut first = request();
        insert_leading(&mut first, Message::system("be brief"));
        let first_rendered = render(&first).unwrap();

        let mut second = Request::new("gpt-5", vec![]);
        second.messages = vec![
            Turn::Span(first_rendered.prefix.clone()),
            Turn::Value(Message::user("again")),
        ];
        let second_rendered = render(&second).unwrap();

        let old = &first_rendered.prefix.regions["messages"];
        let new = &second_rendered.prefix.regions["messages"];
        assert!(new.starts_with(old), "prefix bytes must not be rewritten");
    }

    #[test]
    fn a_span_from_a_different_model_is_a_typed_refusal() {
        let rendered = render(&request()).unwrap();
        let mut other_model = Request::new("gpt-5-mini", vec![]);
        other_model.messages = vec![
            Turn::Span(rendered.prefix),
            Turn::Value(Message::user("hi")),
        ];
        let error = render(&other_model).unwrap_err();
        assert!(matches!(error, Error::SpanScope { .. }));
    }

    fn chunk(delta: Value) -> Value {
        json!({
            "id": "chatcmpl_1",
            "model": "gpt-5",
            "choices": [{"index": 0, "delta": delta, "finish_reason": null}],
        })
    }

    #[test]
    fn the_first_chunk_starts_the_message() {
        let mut decoder = StreamDecoder::default();
        let events = decoder
            .push_chunk(&chunk(json!({"role": "assistant"})))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], Event::MessageStart { id, model, .. } if id == "chatcmpl_1" && model == "gpt-5")
        );
    }

    #[test]
    fn text_deltas_fold_into_one_block() {
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({"role": "assistant", "content": "he"})))
            .unwrap();
        let events = decoder
            .push_chunk(&chunk(json!({"content": "llo"})))
            .unwrap();
        // No second BlockStart: one open text block across chunks.
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], Event::BlockDelta { index: 0, delta: Delta::Text { content } } if content == "llo")
        );
    }

    #[test]
    fn a_tool_call_opens_a_new_block_and_streams_arguments() {
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({"content": "checking"})))
            .unwrap();
        let events = decoder
            .push_chunk(&chunk(json!({
                "tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":"}}],
            })))
            .unwrap();

        // The tool block opens alongside the text block — which stays open, so
        // later text can still extend it — and arguments start streaming.
        assert!(
            matches!(&events[0], Event::BlockStart { index: 1, block: BlockStart::ToolUse { id, name } } if id == "call_1" && name == "read_file")
        );
        assert!(
            matches!(&events[1], Event::BlockDelta { index: 1, delta: Delta::ToolInputJson { content } } if content == "{\"path\":")
        );

        // Later fragments carry only arguments.
        let events = decoder
            .push_chunk(&chunk(json!({
                "tool_calls": [{"index": 0, "function": {"arguments": "\"/tmp/x\"}"}}],
            })))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], Event::BlockDelta { index: 1, delta: Delta::ToolInputJson { content } } if content == "\"/tmp/x\"}")
        );
    }

    #[test]
    fn text_and_tool_arguments_in_one_delta_stay_in_open_blocks() {
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({
                "tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"pa"}}],
            })))
            .unwrap();
        // One delta carrying both kinds, then a later fragment for the call
        // that the interleaved text sits between.
        let events = decoder
            .push_chunk(&chunk(json!({
                "content": "thinking about it",
                "tool_calls": [{"index": 0, "function": {"arguments": "th\":"}}],
            })))
            .unwrap();
        assert!(matches!(
            &events[0],
            Event::BlockStart {
                index: 1,
                block: BlockStart::Text
            }
        ));
        assert!(
            matches!(&events[2], Event::BlockDelta { index: 0, delta: Delta::ToolInputJson { content } } if content == "th\":")
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::BlockStop { .. })),
            "no block may stop while it is still receiving deltas"
        );

        let events = decoder
            .push_chunk(&chunk(json!({
                "tool_calls": [{"index": 0, "function": {"arguments": "\"/tmp/x\"}"}}],
            })))
            .unwrap();
        assert!(
            matches!(&events[0], Event::BlockDelta { index: 0, delta: Delta::ToolInputJson { content } } if content == "\"/tmp/x\"}")
        );

        // Both blocks are still open, and finish stops them in order.
        let events = decoder.finish();
        assert!(matches!(&events[0], Event::BlockStop { index: 0 }));
        assert!(matches!(&events[1], Event::BlockStop { index: 1 }));
        assert!(matches!(&events[2], Event::MessageStop));
    }

    #[test]
    fn interleaved_tool_arguments_accumulate_into_one_call() {
        let mut decoder = StreamDecoder::default();
        let mut accumulator = crate::event::Accumulator::new();
        let mut feed = |decoder: &mut StreamDecoder, chunk: &Value| {
            for event in decoder.push_chunk(chunk).unwrap() {
                accumulator.push(&event);
            }
        };

        feed(&mut decoder, &chunk(json!({"role": "assistant"})));
        feed(
            &mut decoder,
            &chunk(json!({
                "tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":"}}],
            })),
        );
        feed(&mut decoder, &chunk(json!({"content": "one moment"})));
        feed(
            &mut decoder,
            &chunk(json!({
                "tool_calls": [{"index": 0, "function": {"arguments": "\"/tmp/x\"}"}}],
            })),
        );
        for event in decoder.finish() {
            accumulator.push(&event);
        }

        let completion = accumulator.finish().unwrap();
        let calls: Vec<_> = completion.tool_uses().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].2, &json!({"path": "/tmp/x"}));
        assert_eq!(completion.text(), "one moment");
    }

    #[test]
    fn text_resuming_after_reasoning_keeps_its_place() {
        let mut decoder = StreamDecoder::default();
        let mut accumulator = crate::event::Accumulator::new();
        for delta in [
            json!({"reasoning_content": "a"}),
            json!({"content": "b"}),
            json!({"reasoning_content": "c"}),
            json!({"content": "d"}),
        ] {
            for event in decoder.push_chunk(&chunk(delta)).unwrap() {
                accumulator.push(&event);
            }
        }
        for event in decoder.finish() {
            accumulator.push(&event);
        }

        // Four blocks in wire order — text after reasoning must not fold back
        // into the run that came before it.
        let completion = accumulator.finish().unwrap();
        let kinds: Vec<_> = completion
            .message
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => format!("text:{text}"),
                ContentBlock::Thinking { thinking, .. } => format!("thinking:{thinking}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["thinking:a", "text:b", "thinking:c", "text:d"]);
    }

    #[test]
    fn a_null_error_field_is_not_an_error() {
        let mut decoder = StreamDecoder::default();
        // Gateways that stamp `error: null` on every healthy chunk must not
        // fail the stream on the first frame.
        let mut chunk = chunk(json!({"content": "hi"}));
        chunk["error"] = Value::Null;
        let events = decoder.push_chunk(&chunk).unwrap();
        assert!(matches!(&events[0], Event::MessageStart { .. }));
    }

    #[test]
    fn the_finish_chunk_closes_everything() {
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({"content": "hi"})))
            .unwrap();
        let events = decoder
            .push_chunk(&json!({
                "id": "chatcmpl_1",
                "model": "gpt-5",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            }))
            .unwrap();
        assert!(matches!(&events[0], Event::BlockStop { index: 0 }));
        assert!(matches!(
            &events[1],
            Event::MessageDelta {
                stop_reason: Some(StopReason::EndTurn),
                ..
            }
        ));

        let events = decoder.finish();
        assert_eq!(events.len(), 1, "the finish chunk already closed the block");
        assert!(matches!(&events[0], Event::MessageStop));
    }

    #[test]
    fn the_usage_chunk_reports_totals() {
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({"content": "hi"})))
            .unwrap();
        let events = decoder
            .push_chunk(&json!({
                "id": "chatcmpl_1",
                "model": "gpt-5",
                "choices": [],
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 3,
                    "prompt_tokens_details": {"cached_tokens": 5},
                },
            }))
            .unwrap();
        assert!(
            matches!(&events[0], Event::MessageDelta { usage, .. } if usage.input_tokens == Some(12) && usage.output_tokens == Some(3) && usage.cache_read_input_tokens == Some(5))
        );
    }

    #[test]
    fn the_usage_chunk_reports_reasoning_tokens() {
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({"content": "hi"})))
            .unwrap();
        let events = decoder
            .push_chunk(&json!({
                "id": "chatcmpl_1",
                "model": "gpt-5",
                "choices": [],
                "usage": {
                    "prompt_tokens": 12,
                    "completion_tokens": 3,
                    "completion_tokens_details": {"reasoning_tokens": 7},
                },
            }))
            .unwrap();
        assert!(
            matches!(&events[0], Event::MessageDelta { usage, .. } if usage.reasoning_tokens == Some(7))
        );
    }

    #[test]
    fn an_aggregator_report_carries_cost_and_cache_writes() {
        // OpenRouter-shaped usage: the first-party API sends neither field,
        // and the counts must survive the read rather than be dropped.
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({"content": "hi"})))
            .unwrap();
        let events = decoder
            .push_chunk(&json!({
                "id": "chatcmpl_1",
                "model": "openrouter/auto",
                "choices": [],
                "usage": {
                    "prompt_tokens": 194,
                    "completion_tokens": 2,
                    "prompt_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 100},
                    "cost": 0.95,
                },
            }))
            .unwrap();
        assert!(matches!(
            &events[0],
            Event::MessageDelta { usage, .. }
                if usage.cache_creation_input_tokens == Some(100) && usage.cost == Some(0.95)
        ));
    }

    #[test]
    fn a_report_without_a_cost_leaves_it_unreported() {
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({"content": "hi"})))
            .unwrap();
        let events = decoder
            .push_chunk(&json!({
                "id": "chatcmpl_1",
                "model": "gpt-5",
                "choices": [],
                "usage": {"prompt_tokens": 12, "completion_tokens": 3},
            }))
            .unwrap();
        assert!(matches!(&events[0], Event::MessageDelta { usage, .. } if usage.cost.is_none()));
    }

    #[test]
    fn a_content_filter_finish_is_a_refusal() {
        let mut decoder = StreamDecoder::default();
        decoder
            .push_chunk(&chunk(json!({"content": "no"})))
            .unwrap();
        let events = decoder
            .push_chunk(&json!({
                "id": "chatcmpl_1",
                "model": "gpt-5",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "content_filter"}],
            }))
            .unwrap();
        assert!(matches!(
            &events[1],
            Event::MessageDelta {
                stop_reason: Some(StopReason::Refusal),
                ..
            }
        ));
    }

    #[test]
    fn a_mid_stream_error_frame_is_an_api_error() {
        let mut decoder = StreamDecoder::default();
        let error = decoder
            .push_chunk(&json!({"error": {"type": "server_error", "message": "boom"}}))
            .unwrap_err();
        assert!(error.to_string().contains("boom"));
    }
}
