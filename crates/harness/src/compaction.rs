//! Compacting the conversation a run carries: one call that summarizes it,
//! and the shorter turn list the loop goes on from.
//!
//! This is the loop's own policy, not Core's (`history-abstract.md` H8.5).
//! Nothing here touches the lineage directly: [`compact`] returns the turns
//! the loop sends next, and the lineage becomes those turns the usual way,
//! when the call the turn ends on is committed (H4). Until then a compaction
//! is only a request the loop chose to make.
//!
//! What the loop goes on from is the leading system run, by reference, then
//! the summary, then whatever step is still under way. The system run keeps
//! its id, so the tools and system prompt — the costliest bytes to resend —
//! stay cached across the compaction; everything after them is new.

use std::time::Duration;

use serde_json::json;

use crate::context::Context;
use crate::error::Error;
use crate::mapping::{Content, LLMRequest, Message, Role};
use crate::state::Compaction;

/// The share of the context window, as `NUM / DEN`, a conversation may fill
/// before the loop compacts it. Leaves room for the summary call itself to
/// read everything and still write, and for the step under way to finish.
const THRESHOLD_NUM: u64 = 4;
const THRESHOLD_DEN: u64 = 5;

const SUMMARY_INSTRUCTION: &str = "The context is nearly full, so this conversation is being compacted. Write a summary that will replace every message before this one: you will continue from the summary with no other record of them. Do not call tools; reply with the summary only.

Keep the user's goals and requests, in their own words where the wording matters; decisions made and why; the current state of the work, including environments, files, commands and outcomes that still matter; and anything unfinished, with what you were about to do next. Leave out what no longer matters.";

const SUMMARY_HEADER: &str = "This conversation was compacted to free up context. The summary below replaces the messages that came before it.";

const ACKNOWLEDGE: &str = "Reply with one short sentence confirming you have this summary, then wait for the user's next message. Do not call tools.";

/// Why a compaction happened, as the transcript names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trigger {
    /// Someone asked for it.
    Requested,
    /// The conversation reached the threshold.
    Automatic,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Trigger::Requested => "requested",
            Trigger::Automatic => "automatic",
        }
    }
}

/// Whether a conversation that filled `tokens` of a `window`-token context
/// is due for compaction. Unknown on either side is never due.
pub(crate) fn is_due(tokens: Option<u64>, window: Option<u64>) -> bool {
    match (tokens, window) {
        (Some(tokens), Some(window)) if window > 0 => {
            tokens.saturating_mul(THRESHOLD_DEN) >= window.saturating_mul(THRESHOLD_NUM)
        }
        _ => false,
    }
}

/// Summarizes `messages` and returns the turns to go on from. `tokens_before`
/// is the size that made it due, for the transcript and the log.
///
/// `acknowledge` asks the model to answer the summary with one line and
/// stop. A compaction with nothing after it — a request on its own, with no
/// input — still has to end on a call the loop can commit, and the model
/// would otherwise read the summary as an invitation to resume the work.
///
/// Emits `compaction_start` before the summary call and `compaction` once it
/// has one. The summary call's own stream stays out of the transcript: it is
/// a means, and the summary is shown whole in the `compaction` event.
pub(crate) async fn compact(
    ctx: &Context,
    messages: &[Message],
    trigger: Trigger,
    policy: &Compaction,
    tokens_before: Option<u64>,
    acknowledge: bool,
) -> Result<Vec<Message>, Error> {
    let head = leading_system_run(messages);
    let tail = tail_start(messages, head);
    if tail <= head {
        return Err(Error::CompactionFailed {
            message: "there is nothing before the current step to summarize".to_owned(),
        });
    }

    ctx.emit(json!({ "type": "compaction_start", "trigger": trigger.as_str() }));

    let summary = summarize(ctx, messages, policy.instructions.as_deref()).await?;

    let mut text = format!("{SUMMARY_HEADER}\n\n<summary>\n{summary}\n</summary>");
    if acknowledge {
        text.push_str("\n\n");
        text.push_str(ACKNOWLEDGE);
    }

    let mut compacted = Vec::with_capacity(head + 1 + messages.len() - tail);
    compacted.extend_from_slice(&messages[..head]);
    compacted.push(Message::text(Role::User, text));
    compacted.extend_from_slice(&messages[tail..]);

    tracing::info!(
        trigger = trigger.as_str(),
        tokens_before,
        summarized = tail - head,
        kept = messages.len() - tail,
        "conversation compacted"
    );
    ctx.emit(json!({
        "type": "compaction",
        "trigger": trigger.as_str(),
        "summary": summary,
        "tokensBefore": tokens_before,
        "summarized": tail - head,
    }));
    Ok(compacted)
}

/// Says an automatic compaction did not happen, and why.
pub(crate) fn report_skipped(ctx: &Context, error: &Error) {
    tracing::warn!(%error, "automatic compaction failed; continuing uncompacted");
    ctx.emit(json!({
        "type": "compaction_failed",
        "trigger": Trigger::Automatic.as_str(),
        "error": error.to_string(),
    }));
}

/// Whether `error` is a stop someone asked for. Never swallowed: a loop that
/// carried on past it would open new calls after the user said stop.
pub(crate) fn is_stop(error: &Error) -> bool {
    error.kind() == Some("cancelled")
}

/// How many turns open the conversation as its system prompt.
fn leading_system_run(messages: &[Message]) -> usize {
    messages
        .iter()
        .take_while(|message| matches!(message.role, Role::System))
        .count()
}

/// Where the step still under way begins, which a compaction keeps whole.
///
/// The last assistant turn, when it is waiting on the tool results after
/// it: a `tool_result` cut off from its `tool_use` is a malformed request.
/// Otherwise whatever follows the last assistant turn — notices, results
/// recovered on resume — since a finished reply is what the summary is for.
fn tail_start(messages: &[Message], head: usize) -> usize {
    match messages
        .iter()
        .rposition(|message| matches!(message.role, Role::Assistant))
    {
        Some(at) if at >= head => {
            let waiting = messages[at]
                .content
                .iter()
                .any(|block| matches!(block, Content::ToolUse { .. }));
            if waiting { at } else { at + 1 }
        }
        _ => head,
    }
}

/// The summary call: everything so far, then the instruction.
///
/// Every option is left to the committed baseline, tools included, so the
/// request shares its prefix with the call before it and reads it from the
/// cache — the instruction says not to call tools rather than taking them
/// away, which would move the frontmost bytes.
async fn summarize(
    ctx: &Context,
    messages: &[Message],
    instructions: Option<&str>,
) -> Result<String, Error> {
    let mut instruction = SUMMARY_INSTRUCTION.to_owned();
    if let Some(instructions) = instructions.map(str::trim).filter(|text| !text.is_empty()) {
        instruction.push_str("\n\nThe user asked that the summary also: ");
        instruction.push_str(instructions);
    }

    let mut request = messages.to_vec();
    // Joined to a user turn still being written rather than following it:
    // two user turns in a row are merged by some providers and refused by
    // others. A committed one keeps its content, since editing it would be
    // an edit of the lineage.
    match request.last_mut() {
        Some(last) if matches!(last.role, Role::User) && last.id.is_none() => {
            last.content.push(Content::Text { text: instruction });
        }
        _ => request.push(Message::text(Role::User, instruction)),
    }

    let mut attempt = 0;
    let completion = loop {
        let result = match ctx.stream(LLMRequest::new(request.iter().cloned())) {
            Ok(call) => call.completion().await,
            Err(error) => Err(error),
        };
        match result {
            Ok(completion) => break completion,
            Err(error) if error.is_transient() && attempt < crate::conversational::MAX_RETRIES => {
                let delay_ms = crate::conversational::retry_delay_ms(attempt, 0.0).round() as u64;
                attempt += 1;
                ctx.emit(json!({
                    "type": "retry",
                    "attempt": attempt,
                    "maxRetries": crate::conversational::MAX_RETRIES,
                    "delayMs": delay_ms,
                    "error": error.to_string(),
                }));
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(error) => return Err(error),
        }
    };

    let summary: String = completion
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let summary = summary.trim();
    if summary.is_empty() {
        return Err(Error::CompactionFailed {
            message: "the model wrote no summary".to_owned(),
        });
    }
    Ok(summary.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_use() -> Message {
        Message {
            id: None,
            role: Role::Assistant,
            content: vec![Content::ToolUse {
                id: "t1".into(),
                name: "read".into(),
                input: json!({}),
            }],
        }
    }

    #[test]
    fn due_at_four_fifths_of_the_window_and_never_when_unknown() {
        assert!(!is_due(Some(79), Some(100)));
        assert!(is_due(Some(80), Some(100)));
        assert!(!is_due(None, Some(100)));
        assert!(!is_due(Some(1_000_000), None));
        assert!(!is_due(Some(1), Some(0)));
    }

    #[test]
    fn a_step_waiting_on_tool_results_is_kept_whole() {
        let messages = vec![
            Message::text(Role::System, "sys"),
            Message::text(Role::User, "hi"),
            tool_use(),
            Message::text(Role::User, "result"),
        ];
        assert_eq!(leading_system_run(&messages), 1);
        assert_eq!(tail_start(&messages, 1), 2);
    }

    #[test]
    fn a_finished_reply_is_summarized_and_what_follows_it_kept() {
        let messages = vec![
            Message::text(Role::System, "sys"),
            Message::text(Role::User, "hi"),
            Message::text(Role::Assistant, "hello"),
            Message::text(Role::System, "a notice"),
        ];
        assert_eq!(tail_start(&messages, 1), 3);
    }

    #[test]
    fn a_conversation_with_no_reply_has_nothing_to_summarize() {
        let messages = vec![
            Message::text(Role::System, "sys"),
            Message::text(Role::User, "hi"),
        ];
        assert_eq!(tail_start(&messages, 1), 1);
    }
}
