//! Translation between the neutral types and the Anthropic Messages API.
//!
//! Hand-written rather than derived: the neutral types' own `serde` impls are
//! for persistence, and a provider's wire format is free to drift away from
//! them.

use serde::Serialize;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};

use crate::error::Error;
use crate::event::{BlockStart, Delta, Event};
use crate::span::{RenderedRequest, RenderedSpan, append_element, split_span, wrap_array};
use crate::types::{
    ContentBlock, Effort, Message, ReasoningHistory, Request, Role, StopDetails, StopReason,
    Thinking, ThinkingDisplay, ToolChoice, UsageDelta, leading_system_run,
};

const PROVIDER: &str = "anthropic";

/// What this wire does when a request expresses no preference.
///
/// Current Anthropic models verify the signature on a thinking block they are
/// handed back and reject a turn whose reasoning was edited or dropped, so
/// sending it back in full is the only default that keeps a thinking
/// conversation working.
pub const DEFAULT_REASONING: ReasoningHistory = ReasoningHistory::Full;

/// Renders a [`Request`] to Anthropic's wire format.
///
/// Two regions get spliced rather than reconstructed: `system` (present only
/// once a leading run of system turns has been seen) and `messages`. Both
/// carry forward from a resumed [`crate::Turn::Span`] byte-for-byte — see
/// `crate::span` for why that matters (H5).
pub fn render(request: &Request) -> Result<RenderedRequest, Error> {
    let reasoning = request.reasoning_history.unwrap_or(DEFAULT_REASONING);
    let (span, new_turns) = split_span(&request.messages, PROVIDER, &request.model, reasoning)?;

    let mut system_bytes = span
        .and_then(|span| span.regions.get("system"))
        .cloned()
        .unwrap_or_default();
    let mut messages_bytes = span
        .and_then(|span| span.regions.get("messages"))
        .cloned()
        .unwrap_or_default();

    // Anthropic takes a system prompt only as a top-level field, never as
    // `messages[0]`. A leading run of system turns becomes that field — but
    // only when nothing precedes them; a resumed span already has its head
    // settled, so a system turn appended after it is mid-conversation and
    // stays inline, in place, as an ordinary `role: "system"` entry.
    let leading_system = if span.is_none() {
        leading_system_run(new_turns.iter().copied())
    } else {
        0
    };
    let (leading, rest) = new_turns.split_at(leading_system);

    for message in leading {
        append_element(
            &mut system_bytes,
            &json!({"type": "text", "text": message.text()}),
        );
    }
    for message in rest {
        // A turn that renders to nothing is dropped rather than sent empty:
        // reachable only when a reasoning-only assistant turn meets a policy
        // that drops reasoning, and `content: []` is rejected outright.
        match message_to_json(message, reasoning) {
            Some(value) => append_element(&mut messages_bytes, &value),
            None => continue,
        }
    }

    let prefix = RenderedSpan {
        provider: PROVIDER.into(),
        model: request.model.clone(),
        reasoning,
        regions: [
            ("system".to_string(), system_bytes.clone()),
            ("messages".to_string(), messages_bytes.clone()),
        ]
        .into_iter()
        .collect(),
    };

    let body = build_body(request, &messages_bytes, &system_bytes)?;
    Ok(RenderedRequest { body, prefix })
}

fn build_body(
    request: &Request,
    messages_bytes: &[u8],
    system_bytes: &[u8],
) -> Result<Vec<u8>, Error> {
    let mut head = request_head(request);
    // `messages`/`system` are spliced separately below; a colliding `extra`
    // key must lose to that, same as every other modelled field.
    head.remove("messages");
    head.remove("system");

    let messages_raw = RawValue::from_string(wrap_array(messages_bytes)?)
        .map_err(|error| Error::Decode(format!("rendered messages region: {error}")))?;
    let system_raw = if system_bytes.is_empty() {
        None
    } else {
        Some(
            RawValue::from_string(wrap_array(system_bytes)?)
                .map_err(|error| Error::Decode(format!("rendered system region: {error}")))?,
        )
    };

    #[derive(Serialize)]
    struct Body<'a> {
        #[serde(flatten)]
        head: Map<String, Value>,
        messages: &'a RawValue,
        #[serde(skip_serializing_if = "Option::is_none")]
        system: Option<&'a RawValue>,
    }

    serde_json::to_vec(&Body {
        head,
        messages: &messages_raw,
        system: system_raw.as_deref(),
    })
    .map_err(|error| Error::Decode(format!("failed to serialize request body: {error}")))
}

/// Every field except `messages`/`system`, which the caller splices in
/// separately so their bytes never round-trip through this map.
fn request_head(request: &Request) -> Map<String, Value> {
    let mut body = Map::new();

    // Caller-supplied passthrough first, so modelled fields always win.
    for (key, value) in &request.extra {
        body.insert(key.clone(), value.clone());
    }

    body.insert("model".into(), json!(request.model));
    body.insert("max_tokens".into(), json!(request.max_tokens));
    body.insert("stream".into(), json!(true));

    if !request.tools.is_empty() {
        body.insert(
            "tools".into(),
            Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.input_schema,
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
                ToolChoice::Auto => json!({"type": "auto"}),
                ToolChoice::Any => json!({"type": "any"}),
                ToolChoice::None => json!({"type": "none"}),
                ToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
            },
        );
    }
    if let Some(thinking) = request.thinking {
        body.insert(
            "thinking".into(),
            match thinking {
                // `budget_tokens` is rejected on current models; adaptive is
                // the only on-mode. `display` defaults to omitted, which
                // yields thinking blocks with empty text.
                Thinking::Adaptive { display } => json!({
                    "type": "adaptive",
                    "display": match display {
                        ThinkingDisplay::Omitted => "omitted",
                        ThinkingDisplay::Summarized => "summarized",
                    },
                }),
                Thinking::Disabled => json!({"type": "disabled"}),
            },
        );
    }
    if let Some(effort) = request.effort {
        // Effort is nested under output_config, not top level.
        let level = match effort {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        };
        match body.get_mut("output_config") {
            Some(Value::Object(config)) => {
                config.insert("effort".into(), json!(level));
            }
            _ => {
                body.insert("output_config".into(), json!({ "effort": level }));
            }
        }
    }
    // Omitted when unset: current models reject these outright rather than
    // treating an explicit default as a no-op.
    if let Some(temperature) = request.sampling.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(top_p) = request.sampling.top_p {
        body.insert("top_p".into(), json!(top_p));
    }
    if let Some(top_k) = request.sampling.top_k {
        body.insert("top_k".into(), json!(top_k));
    }
    if !request.stop_sequences.is_empty() {
        body.insert("stop_sequences".into(), json!(request.stop_sequences));
    }

    body
}

/// `None` when the policy leaves the turn with no content at all — see the
/// call site in [`render`].
fn message_to_json(message: &Message, reasoning: ReasoningHistory) -> Option<Value> {
    let content: Vec<Value> = message
        .content
        .iter()
        .filter_map(|block| block_to_json(block, reasoning))
        .collect();

    // Only a turn the policy emptied is dropped. A turn that arrived empty is
    // rendered as it is and refused by the provider, the way it always was —
    // swallowing it here would turn a caller's mistake into a silent one.
    if content.is_empty() && !message.content.is_empty() {
        return None;
    }

    Some(json!({
        "role": match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        },
        "content": Value::Array(content),
    }))
}

/// `None` when the block has no wire form under `reasoning`.
fn block_to_json(block: &ContentBlock, reasoning: ReasoningHistory) -> Option<Value> {
    let value = match block {
        ContentBlock::Text { text } => json!({"type": "text", "text": text}),
        ContentBlock::Thinking {
            thinking,
            signature,
        } => match reasoning {
            ReasoningHistory::Omitted => return None,
            ReasoningHistory::Text => json!({"type": "thinking", "thinking": thinking}),
            ReasoningHistory::Full => {
                let mut value = json!({"type": "thinking", "thinking": thinking});
                if let Some(signature) = signature {
                    value["signature"] = json!(signature);
                }
                value
            }
        },
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
        // Echoed back exactly as it arrived — see the Unknown note on ContentBlock.
        ContentBlock::Unknown { raw } => raw.clone(),
    };
    Some(value)
}

/// Maps one SSE frame to a neutral event.
///
/// `Ok(None)` means the frame carries no information a caller needs (a
/// keepalive ping).
pub(super) fn parse_event(frame: &Value) -> Result<Option<Event>, Error> {
    let kind = frame
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Decode("stream frame has no `type`".into()))?;

    let event = match kind {
        "ping" => return Ok(None),
        "error" => {
            let error = frame.get("error");
            return Err(Error::Api {
                status: None,
                kind: error
                    .and_then(|error| error.get("type"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                message: error
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("stream reported an error")
                    .to_owned(),
                request_id: None,
            });
        }
        "message_start" => {
            let message = frame
                .get("message")
                .ok_or_else(|| Error::Decode("message_start has no `message`".into()))?;
            Event::MessageStart {
                id: string_at(message, "id"),
                model: string_at(message, "model"),
                usage: usage_from(message.get("usage")),
            }
        }
        "content_block_start" => Event::BlockStart {
            index: index_of(frame)?,
            block: block_start_from(frame.get("content_block")),
        },
        "content_block_delta" => Event::BlockDelta {
            index: index_of(frame)?,
            delta: delta_from(frame.get("delta")),
        },
        "content_block_stop" => Event::BlockStop {
            index: index_of(frame)?,
        },
        "message_delta" => {
            let delta = frame.get("delta");
            Event::MessageDelta {
                stop_reason: delta
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(stop_reason_from),
                // Populated only alongside a refusal.
                stop_details: delta
                    .and_then(|delta| delta.get("stop_details"))
                    .filter(|value| !value.is_null())
                    .map(|value| StopDetails {
                        category: value
                            .get("category")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        explanation: value
                            .get("explanation")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    }),
                usage: usage_from(frame.get("usage")),
            }
        }
        "message_stop" => Event::MessageStop,
        _ => Event::Unknown { raw: frame.clone() },
    };

    Ok(Some(event))
}

fn index_of(frame: &Value) -> Result<usize, Error> {
    frame
        .get("index")
        .and_then(Value::as_u64)
        .map(|index| index as usize)
        .ok_or_else(|| Error::Decode("content block frame has no `index`".into()))
}

fn string_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn block_start_from(block: Option<&Value>) -> BlockStart {
    let Some(block) = block else {
        return BlockStart::Unknown { raw: Value::Null };
    };
    match block.get("type").and_then(Value::as_str) {
        Some("text") => BlockStart::Text,
        Some("thinking") => BlockStart::Thinking,
        Some("tool_use") => BlockStart::ToolUse {
            id: string_at(block, "id"),
            name: string_at(block, "name"),
        },
        _ => BlockStart::Unknown { raw: block.clone() },
    }
}

fn delta_from(delta: Option<&Value>) -> Delta {
    let Some(delta) = delta else {
        return Delta::Unknown { raw: Value::Null };
    };
    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => Delta::Text {
            content: string_at(delta, "text"),
        },
        Some("thinking_delta") => Delta::Thinking {
            content: string_at(delta, "thinking"),
        },
        Some("signature_delta") => Delta::ThinkingSignature {
            content: string_at(delta, "signature"),
        },
        Some("input_json_delta") => Delta::ToolInputJson {
            content: string_at(delta, "partial_json"),
        },
        _ => Delta::Unknown { raw: delta.clone() },
    }
}

fn stop_reason_from(reason: &str) -> StopReason {
    match reason {
        "end_turn" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        "tool_use" => StopReason::ToolUse,
        "pause_turn" => StopReason::PauseTurn,
        "refusal" => StopReason::Refusal,
        "model_context_window_exceeded" => StopReason::ContextWindowExceeded,
        other => StopReason::Other(other.to_owned()),
    }
}

/// Reads only the fields the report actually carries: an absent count and a
/// reported zero mean different things.
fn usage_from(usage: Option<&Value>) -> UsageDelta {
    let Some(usage) = usage else {
        return UsageDelta::default();
    };
    let count = |key: &str| usage.get(key).and_then(Value::as_u64);
    UsageDelta {
        input_tokens: count("input_tokens"),
        output_tokens: count("output_tokens"),
        cache_read_input_tokens: count("cache_read_input_tokens"),
        cache_creation_input_tokens: count("cache_creation_input_tokens"),
        // Not reported separately by this API.
        reasoning_tokens: None,
        // This API returns tokens only; a configured price is the only cost.
        cost: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Sampling, Turn};

    fn request() -> Request {
        Request::new("claude-opus-5", vec![Message::user("hi")])
    }

    /// Test-only convenience: renders and parses the body back into a
    /// `Value` for structural assertions. Production code never does this —
    /// see the `*_prefix_bytes_survive_a_round_trip` tests below for the
    /// assertions that actually matter (H5).
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

    /// The constraint that decides how a consumer may carry a
    /// mid-conversation note: it cannot be a system turn.
    ///
    /// Only a *leading* run of system turns becomes the top-level `system`
    /// field. One appended later stays inline in `messages`, where this API
    /// accepts `user` and `assistant` and refuses anything else — so a note
    /// added at turn nine has to travel as a user turn, marked by its content
    /// rather than by its role.
    #[test]
    fn a_system_turn_after_the_head_stays_inline_where_the_api_will_refuse_it() {
        let mut request = request();
        push(&mut request, Message::system("added later"));
        let body = request_body(&request);

        assert!(body.get("system").is_none());
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "system");
    }

    #[test]
    fn reasoning_goes_back_whole_by_default() {
        let mut request = request();
        push(
            &mut request,
            Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "hmm".into(),
                    signature: Some("sig".into()),
                },
                ContentBlock::text("done"),
            ]),
        );
        push(&mut request, Message::user("and again"));
        let body = request_body(&request);

        let blocks = &body["messages"][1]["content"];
        assert_eq!(blocks[0]["thinking"], json!("hmm"));
        assert_eq!(blocks[0]["signature"], json!("sig"));
    }

    #[test]
    fn text_only_reasoning_leaves_the_signature_behind() {
        let mut request = request();
        request.reasoning_history = Some(ReasoningHistory::Text);
        push(
            &mut request,
            Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "hmm".into(),
                    signature: Some("sig".into()),
                },
                ContentBlock::text("done"),
            ]),
        );
        push(&mut request, Message::user("and again"));
        let body = request_body(&request);

        let blocks = &body["messages"][1]["content"];
        assert_eq!(blocks[0]["thinking"], json!("hmm"));
        assert!(blocks[0].get("signature").is_none());
    }

    #[test]
    fn omitted_reasoning_leaves_the_rest_of_the_turn_intact() {
        let mut request = request();
        request.reasoning_history = Some(ReasoningHistory::Omitted);
        push(
            &mut request,
            Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "hmm".into(),
                    signature: Some("sig".into()),
                },
                ContentBlock::text("done"),
            ]),
        );
        push(&mut request, Message::user("and again"));
        let body = request_body(&request);

        let blocks = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["text"], json!("done"));
    }

    #[test]
    fn a_turn_left_empty_by_the_policy_is_dropped_rather_than_sent() {
        // Reachable when a reasoning model runs out of tokens before writing
        // anything: `content: []` is rejected outright, so the turn goes.
        let mut request = request();
        request.reasoning_history = Some(ReasoningHistory::Omitted);
        push(
            &mut request,
            Message::assistant(vec![ContentBlock::Thinking {
                thinking: "hmm".into(),
                signature: Some("sig".into()),
            }]),
        );
        push(&mut request, Message::user("and again"));
        let body = request_body(&request);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["content"][0]["text"], json!("and again"));
    }

    #[test]
    fn a_turn_that_arrived_empty_is_still_sent_as_it_is() {
        // Not this feature's business: an empty turn is a caller's mistake,
        // and the provider's refusal is how it has always been reported.
        let mut request = request();
        push(&mut request, Message::assistant(Vec::new()));
        let body = request_body(&request);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["content"], json!([]));
    }

    #[test]
    fn bare_request_sends_nothing_it_was_not_given() {
        let body = request_body(&request());
        let object = body.as_object().unwrap();
        assert_eq!(object["model"], json!("claude-opus-5"));
        assert_eq!(object["stream"], json!(true));
        for absent in [
            "temperature",
            "top_p",
            "top_k",
            "thinking",
            "output_config",
            "tools",
            "tool_choice",
            "system",
            "stop_sequences",
        ] {
            assert!(
                !object.contains_key(absent),
                "unexpected `{absent}` in body"
            );
        }
    }

    #[test]
    fn effort_nests_under_output_config() {
        let mut request = request();
        request.effort = Some(Effort::XHigh);
        let body = request_body(&request);
        assert_eq!(body["output_config"]["effort"], json!("xhigh"));
        assert!(body.get("effort").is_none());
    }

    #[test]
    fn sampling_is_sent_only_when_set() {
        let mut request = request();
        request.sampling = Sampling {
            temperature: Some(0.5),
            ..Sampling::default()
        };
        let body = request_body(&request);
        assert_eq!(body["temperature"], json!(0.5));
        assert!(body.get("top_p").is_none());
    }

    #[test]
    fn modelled_fields_win_over_extra() {
        let mut request = request();
        request
            .extra
            .insert("model".into(), json!("something-else"));
        request
            .extra
            .insert("service_tier".into(), json!("standard"));
        let body = request_body(&request);
        assert_eq!(body["model"], json!("claude-opus-5"));
        assert_eq!(body["service_tier"], json!("standard"));
    }

    #[test]
    fn leading_system_messages_become_the_system_field() {
        let mut request = request();
        insert_leading(&mut request, Message::system("be brief"));
        let body = request_body(&request);
        assert_eq!(
            body["system"],
            json!([{"type": "text", "text": "be brief"}])
        );
        assert_eq!(body["messages"][0]["role"], json!("user"));
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_mid_conversation_system_message_is_sent_inline() {
        let mut request = request();
        push(&mut request, Message::system("terse mode"));
        let body = request_body(&request);
        assert!(body.get("system").is_none());
        assert_eq!(body["messages"][1]["role"], json!("system"));
        assert_eq!(
            body["messages"][1]["content"],
            json!([{"type": "text", "text": "terse mode"}])
        );
    }

    #[test]
    fn turn_one_has_an_empty_prefix_and_no_stray_comma() {
        // §4's default path: nothing committed yet, so the resumed prefix is
        // an empty span. Splicing it must still produce valid JSON.
        let empty_prefix = render(&Request::new("claude-opus-5", vec![]))
            .unwrap()
            .prefix;
        let mut resumed = Request::new("claude-opus-5", vec![]);
        resumed.messages = vec![Turn::Span(empty_prefix), Turn::Value(Message::user("hi"))];
        let rendered = render(&resumed).unwrap();
        let body: Value = serde_json::from_slice(&rendered.body).unwrap();
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}])
        );
    }

    #[test]
    fn a_resumed_prefix_splices_byte_identically() {
        // H5's stated test: render, capture the prefix, render again
        // splicing it in, and the prefix region must match byte for byte —
        // not just structurally.
        let mut first = request();
        insert_leading(&mut first, Message::system("be brief"));
        let first_rendered = render(&first).unwrap();

        let mut second = Request::new("claude-opus-5", vec![]);
        second.messages = vec![
            Turn::Span(first_rendered.prefix.clone()),
            Turn::Value(Message::user("again")),
        ];
        let second_rendered = render(&second).unwrap();

        assert_eq!(
            second_rendered.prefix.regions["system"],
            first_rendered.prefix.regions["system"],
        );
        // The messages region is a strict byte-extension: the old content
        // unchanged, followed by exactly one new comma-joined element.
        let old = &first_rendered.prefix.regions["messages"];
        let new = &second_rendered.prefix.regions["messages"];
        assert!(new.starts_with(old), "prefix bytes must not be rewritten");
    }

    #[test]
    fn a_span_from_a_different_model_is_a_typed_refusal() {
        let rendered = render(&request()).unwrap();
        let mut other_model = Request::new("claude-haiku-4-5", vec![]);
        other_model.messages = vec![
            Turn::Span(rendered.prefix),
            Turn::Value(Message::user("hi")),
        ];
        let error = render(&other_model).unwrap_err();
        assert!(matches!(error, Error::SpanScope { .. }));
    }

    #[test]
    fn ping_carries_nothing() {
        assert!(parse_event(&json!({"type": "ping"})).unwrap().is_none());
    }

    #[test]
    fn unrecognised_frames_pass_through() {
        let event = parse_event(&json!({"type": "invented_later"}))
            .unwrap()
            .unwrap();
        assert!(matches!(event, Event::Unknown { .. }));
    }

    #[test]
    fn stream_error_frame_becomes_an_api_error() {
        let error = parse_event(&json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "overloaded"},
        }))
        .unwrap_err();
        assert!(error.is_transient(), "overload should read as transient");
    }

    #[test]
    fn refusal_carries_its_details() {
        let event = parse_event(&json!({
            "type": "message_delta",
            "delta": {"stop_reason": "refusal", "stop_details": {"category": "cyber"}},
            "usage": {"output_tokens": 12},
        }))
        .unwrap()
        .unwrap();
        match event {
            Event::MessageDelta {
                stop_reason,
                stop_details,
                usage,
            } => {
                assert_eq!(stop_reason, Some(StopReason::Refusal));
                assert_eq!(stop_details.unwrap().category.as_deref(), Some("cyber"));
                assert_eq!(usage.output_tokens, Some(12));
            }
            other => panic!("expected a message delta, got {other:?}"),
        }
    }
}
