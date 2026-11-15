//! The loop every run executes: a system prompt, a tool loop, a commit, and
//! a title for a session that has none.
//!
//! History does not advance on its own: a call that is streamed and never
//! committed leaves `history` returning the same thing next time, and every
//! turn would then start from an empty lineage. Committing is this loop's
//! job. Only the *last* call is committed, and that is enough — each call
//! carries every earlier turn by value, so the final one's turn list is the
//! whole exchange, tool calls and results included.

use std::time::Duration;

use futures_util::future::maybe_done;
use serde_json::{Value, json};

use crate::context::{Call, Context};
use crate::error::Error;
use crate::mapping::{
    CommitOptions, Content, LLMRequest, Message, Role, Thinking, ToolResult, WireTurn,
};

const TITLE_INSTRUCTION: &str = "Generate a concise title for the user's prompt. Reply with only the title, with no quotes or explanation.";

/// Matches the limit `session.set_title` enforces server-side.
const MAX_TITLE_LENGTH: usize = 200;

const SYSTEM_PROMPT: &str = "You are Panit, an agent that does work in the user's bound environments through tools.
`bound_environments` lists what you can reach; every other environment tool takes `execute_in`, and `exec`/`start` take `cwd`, which applies to that call only and never persists.
`patch` operations run in order and are not atomic. A finished command is a result even when its exit is nonzero.
Act with tools when a call answers the question; be direct and concise, and report what you did.";

const MAX_RETRIES: u32 = 3;
const BASE_DELAY_MS: f64 = 1000.0;
const MAX_DELAY_MS: f64 = 30000.0;

/// Runs one turn of a conversation.
pub async fn execute(ctx: Context, input: Vec<Message>) -> Result<(), Error> {
    // Title generation runs alongside the turn. It must not delay the first
    // visible model event, and it is finished after the response so the title
    // is stored before the run ends. A turn that fails abandons it.
    let title = maybe_done(generate_title(&ctx, &input));
    let turn = converse(&ctx, &input);
    tokio::pin!(title, turn);

    let mut title_running = true;
    let call = loop {
        tokio::select! {
            biased;
            ended = &mut turn => break ended?,
            () = &mut title, if title_running => title_running = false,
        }
    };

    // After the loop, so what is adopted is the call the conversation
    // actually ended on. The call is passed whole — it already holds exactly
    // what was sent and exactly what came back (`proposal.md` §6.1).
    ctx.commit(&call, CommitOptions::default()).await?;

    title.as_mut().await;
    if let Some(Some(title)) = title.as_mut().take_output() {
        ctx.emit(json!({ "type": "session_title", "title": title }));
    }
    Ok(())
}

/// The tool loop. Returns the call the conversation ended on.
async fn converse(ctx: &Context, input: &[Message]) -> Result<Call, Error> {
    let history = ctx.history();

    // The default prompt, sent exactly once. It is committed with the first
    // call, so later turns inherit it through `history` instead of resending
    // it by value — which would read as a changed prefix (`scaffold_mismatch`)
    // and invalidate every cached byte behind it. A thread seeded from before
    // this prompt existed keeps no prompt, for the same reason: rewriting its
    // head would do the same to its cache.
    //
    // A session in a project composes its own head from its plugins; anything
    // else keeps the built-in prompt.
    let mut messages = if history.is_empty() {
        let head = invoke_if_granted(ctx, "session.system_head", Value::Null).await?;
        let head = match head {
            Some(Value::String(head)) => head,
            _ => SYSTEM_PROMPT.to_owned(),
        };
        vec![Message::text(Role::System, head)]
    } else {
        history
    };

    // A previous run can leave a committed assistant tool call without its
    // user tool-result turn. Recover it before appending new input; otherwise
    // the first request of this run is malformed and the provider rejects it.
    let pending = messages.last().map(tool_calls_in).unwrap_or_default();
    if !pending.is_empty() {
        let results = dispatch_tool_calls(ctx, pending).await?;
        messages.push(Message {
            id: None,
            role: Role::User,
            content: results,
        });

        // The results above were produced now, not when the model asked for
        // them — the world may have moved on in between. Say so as a
        // mid-conversation system note rather than leaving the model to assume
        // the results are as fresh as its call.
        messages.push(Message::text(
            Role::System,
            "The previous run was interrupted after these tool calls were made; the results above were produced just now, on resume, and may reflect a state that has changed since the calls were issued.",
        ));
    }
    // Notices and plugin hook messages land before the user's message, so
    // their words stay last.
    messages.extend(drain_notices(ctx, "message").await?);
    messages.extend(input.iter().cloned());

    loop {
        let call = stream_with_retry(ctx, &messages).await?;
        let completion = call.completion().await?;
        let calls = tool_calls_in(&completion.message);
        if calls.is_empty() {
            // Notices, or messages, waiting on the end of the turn resume the
            // run for one more call; the caller caps how often.
            let due = drain_notices(ctx, "end_turn").await?;
            if due.is_empty() {
                return Ok(call);
            }
            messages.push(completion.message);
            messages.extend(due);
            continue;
        }

        // The assistant's turn goes back exactly as it came, tool_use blocks
        // and all: a tool_result whose tool_use is missing or edited is a
        // malformed request, and the pairing is checked before anything is
        // sent.
        messages.push(completion.message);

        let results = dispatch_tool_calls(ctx, calls).await?;
        messages.push(Message {
            id: None,
            role: Role::User,
            content: results,
        });
        messages.extend(drain_notices(ctx, "tool_result").await?);
    }
}

/// One tool call the model asked for.
struct ToolUse {
    id: String,
    name: String,
    input: Value,
}

fn tool_calls_in(message: &Message) -> Vec<ToolUse> {
    if !matches!(message.role, Role::Assistant) {
        return Vec::new();
    }
    message
        .content
        .iter()
        .filter_map(|block| match block {
            Content::ToolUse { id, name, input } => Some(ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// Runs `calls` in order, emitting each call and its result, and returns the
/// `tool_result` blocks that answer them.
async fn dispatch_tool_calls(ctx: &Context, calls: Vec<ToolUse>) -> Result<Vec<Content>, Error> {
    let mut results = Vec::with_capacity(calls.len());
    for call in calls {
        ctx.emit(json!({
            "type": "tool_call",
            "id": call.id,
            "name": call.name,
            "input": call.input,
        }));

        // `None` means the tool was never granted — a move this loop does not
        // have. Saying so as a tool_result keeps the pairing intact.
        let result = ctx
            .invoke_tool(&call.name, call.input)
            .await?
            .unwrap_or_else(|| ToolResult {
                content: format!("`{}` is not a tool this run was granted", call.name),
                is_error: true,
            });

        ctx.emit(json!({
            "type": "tool_result",
            "id": call.id,
            "name": call.name,
            "content": result.content,
            "isError": result.is_error,
        }));
        results.push(Content::ToolResult {
            tool_use_id: call.id,
            content: result.content,
            is_error: result.is_error,
        });
    }
    Ok(results)
}

/// A granted function's answer, or `None` when this run was not given it.
async fn invoke_if_granted(
    ctx: &Context,
    name: &str,
    input: Value,
) -> Result<Option<Value>, Error> {
    if !ctx.has_function(name) {
        return Ok(None);
    }
    ctx.invoke_function(name, input).await.map(Some)
}

/// What the host has for the model at `moment` ("message", "tool_result" or
/// "end_turn"): plugin notices, and what was said to the session while the
/// run was under way, as turns ready to append. A run with no plugins has
/// none.
///
/// The host may answer with the turns alone, or with `messages` and the
/// `events` that show them in the transcript. Those are emitted here, so
/// they land where the turns do: after the tool results before them.
async fn drain_notices(ctx: &Context, moment: &str) -> Result<Vec<Message>, Error> {
    const NAME: &str = "plugins.drain";

    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Due {
        Messages(Vec<Message>),
        Shown {
            #[serde(default)]
            messages: Vec<Message>,
            #[serde(default)]
            events: Vec<Value>,
        },
    }

    let due = match invoke_if_granted(ctx, NAME, Value::String(moment.to_owned())).await? {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(due) => serde_json::from_value(due).map_err(|error| Error::FunctionFailed {
            name: NAME.to_owned(),
            message: format!("its notices are not messages: {error}"),
        })?,
    };
    match due {
        Due::Messages(messages) => Ok(messages),
        Due::Shown { messages, events } => {
            for event in events {
                ctx.emit(event);
            }
            Ok(messages)
        }
    }
}

fn retry_delay_ms(attempt: u32, retry_after_ms: f64) -> f64 {
    if retry_after_ms > 0.0 {
        return retry_after_ms.min(MAX_DELAY_MS);
    }
    let exponential = BASE_DELAY_MS * 2f64.powi(attempt as i32);
    let jitter = exponential * (0.5 + rand::random::<f64>() * 0.5);
    jitter.min(MAX_DELAY_MS)
}

/// Streams one call into the transcript, trying again after a transient
/// failure. Returns the call that ended cleanly.
async fn stream_with_retry(ctx: &Context, messages: &[Message]) -> Result<Call, Error> {
    // What the previous attempt failed with, and how long its failure asks
    // the next one to wait.
    let mut last_failure: Option<(String, f64)> = None;

    for attempt in 0..=MAX_RETRIES {
        if let Some((error, retry_after_ms)) = &last_failure {
            let delay_ms = retry_delay_ms(attempt - 1, *retry_after_ms).round() as u64;
            ctx.emit(json!({
                "type": "retry",
                "attempt": attempt,
                "maxRetries": MAX_RETRIES,
                "delayMs": delay_ms,
                "error": error,
            }));
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let error = match stream_once(ctx, messages).await {
            Ok(call) => return Ok(call),
            Err(error) => error,
        };
        if !error.is_transient() || attempt == MAX_RETRIES {
            return Err(error);
        }
        let retry_after_ms = if error.status() == Some(429) {
            BASE_DELAY_MS * 2f64.powi(attempt as i32 + 1)
        } else {
            0.0
        };
        last_failure = Some((error.to_string(), retry_after_ms));
    }
    unreachable!("the last attempt returns either its call or its failure")
}

async fn stream_once(ctx: &Context, messages: &[Message]) -> Result<Call, Error> {
    let call = ctx.stream(LLMRequest::new(messages.iter().cloned()))?;
    while let Some(event) = call.next().await? {
        ctx.emit_model_event(&event);
    }
    Ok(call)
}

/// The instruction and the opener share one user turn on purpose. Sent as a
/// system instruction followed by the raw user turn, a small model reads the
/// opener as something to answer instead of something to name. Labelling it
/// "The prompt:" keeps it data.
fn title_prompt(input: &[Message]) -> Option<String> {
    let opener = input
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let opener = opener.trim();
    (!opener.is_empty()).then(|| format!("{TITLE_INSTRUCTION}\n\nThe prompt: {opener}"))
}

/// Some local models answer the prompt instead of titling it and keep writing
/// a whole paragraph. The instruction asks for one line, so that is where the
/// title ends; the rest is dropped before it can trip the server's limit.
fn normalize_title(raw: &str) -> String {
    const QUOTES: &[char] = &['"', '\'', '“', '”', '‘', '’'];
    let first_line = raw.trim().lines().next().unwrap_or_default().trim();
    let unquoted = first_line.trim_matches(QUOTES).trim();
    let capped: String = unquoted.chars().take(MAX_TITLE_LENGTH).collect();
    capped.trim().to_owned()
}

/// Titles a session that has no title yet.
///
/// Runs on every turn of such a session, so a first attempt that failed is
/// made up on the next turn. Each path logs why it ended, so an untitled
/// session can be explained from the console. Best effort: a failure here
/// must not affect the main response.
async fn generate_title(ctx: &Context, input: &[Message]) -> Option<String> {
    if !ctx.has_function("session.get_title") {
        tracing::warn!(outcome = "skipped", reason = "not_granted", "session title");
        return None;
    }
    let Some(prompt) = title_prompt(input) else {
        tracing::debug!(outcome = "skipped", reason = "no_input", "session title");
        return None;
    };

    match request_title(ctx, prompt).await {
        Ok(title) => title,
        Err(error) => {
            tracing::warn!(
                outcome = "failed",
                reason = "error",
                kind = ?error.kind(),
                %error,
                "session title"
            );
            None
        }
    }
}

async fn request_title(ctx: &Context, prompt: String) -> Result<Option<String>, Error> {
    let current = ctx
        .invoke_function("session.get_title", Value::Null)
        .await?;
    if !current.is_null() {
        tracing::debug!(
            outcome = "skipped",
            reason = "already_titled",
            "session title"
        );
        return Ok(None);
    }

    let call = ctx.stream(LLMRequest {
        messages: vec![WireTurn::from(Message::text(Role::User, prompt))],
        tools: Some(Vec::new()),
        max_tokens: Some(64),
        // A reasoning model spends the title budget on thinking before it
        // writes a single visible token, and a 64-token cap then truncates the
        // call with no text at all. This side call has no use for reasoning.
        thinking: Some(Thinking::Disabled),
        // The title is the first line. Stopping there keeps a model that
        // answers the prompt instead of titling it from spending the rest of
        // the budget on prose.
        stop_sequences: Some(vec!["\n".to_owned()]),
        ..LLMRequest::default()
    })?;

    // The title's own stream stays out of the transcript.
    let completion = call.completion().await?;
    let text: String = completion
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let title = normalize_title(&text);
    if title.is_empty() {
        tracing::warn!(
            outcome = "failed",
            reason = "empty_completion",
            "session title"
        );
        return Ok(None);
    }

    // The first check can become stale while the title request runs. Do not
    // replace a title another run or the user set in the meantime.
    let latest = ctx
        .invoke_function("session.get_title", Value::Null)
        .await?;
    if !latest.is_null() {
        tracing::debug!(
            outcome = "skipped",
            reason = "titled_meanwhile",
            "session title"
        );
        return Ok(None);
    }
    ctx.invoke_function("session.set_title", Value::String(title.clone()))
        .await?;
    tracing::info!(outcome = "generated", "session title");
    Ok(Some(title))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_its_first_line_without_quotes() {
        assert_eq!(
            normalize_title("  “Fix the build”\nand more prose"),
            "Fix the build"
        );
        assert_eq!(normalize_title("'quoted'\r\nrest"), "quoted");
        assert_eq!(normalize_title("\n\n"), "");
    }

    #[test]
    fn a_title_is_capped_by_characters_not_bytes() {
        let long = "é".repeat(MAX_TITLE_LENGTH + 50);
        assert_eq!(normalize_title(&long).chars().count(), MAX_TITLE_LENGTH);
    }

    #[test]
    fn an_opener_with_no_text_has_no_title_prompt() {
        assert!(title_prompt(&[Message::text(Role::User, "   ")]).is_none());
        let prompt = title_prompt(&[Message::text(Role::User, "hello")]).unwrap();
        assert!(prompt.ends_with("The prompt: hello"));
    }
}
