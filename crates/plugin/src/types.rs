//! `interface types` of `faber:plugin@1.0.0`, in Rust.
//!
//! One-to-one with `wit/plugin.wit`. Serialized names are the WIT ones
//! (kebab-case cases, snake-free records), so what crosses the API to a client
//! reads the same as the contract it came from.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON text in the WIT; parsed here, because the core checks it parses.
pub type Json = Value;

/// Mirrors `llm::Role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// The plugin-safe subset of `llm::ContentBlock`. Grows only after `llm` does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Content {
    Text { text: String },
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Content::Text { text: text.into() }
    }

    pub fn as_text(&self) -> &str {
        match self {
            Content::Text { text } => text,
        }
    }
}

/// Every text block of a content list, joined. What a wire with a single
/// string tool result carries.
pub fn joined(content: &[Content]) -> String {
    content
        .iter()
        .map(Content::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bytes of text in a content list — what every text cap is measured in.
pub fn text_len(content: &[Content]) -> usize {
    content.iter().map(|block| block.as_text().len()).sum()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// `assistant` is refused for now.
    pub role: Role,
    pub content: Vec<Content>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: vec![Content::text(text)],
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Message {
            role: Role::System,
            content: vec![Content::text(text)],
        }
    }

    pub fn text(&self) -> String {
        joined(&self.content)
    }

    pub fn into_llm(self) -> llm::Message {
        llm::Message {
            role: match self.role {
                Role::User => llm::Role::User,
                Role::Assistant => llm::Role::Assistant,
                Role::System => llm::Role::System,
            },
            content: self
                .content
                .into_iter()
                .map(|block| match block {
                    Content::Text { text } => llm::ContentBlock::Text { text },
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The model's `tool_use` id.
    pub id: String,
    /// The declared name, without the plugin prefix.
    pub name: String,
    /// Already checked against the tool's input schema.
    pub input: Json,
}

/// Who is responsible for a failed call. A closed set: a new case must name a
/// different responsible party.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Failure {
    /// The model's call.
    BadRequest,
    /// The plugin.
    HandlerFailure,
    /// Neither: a machine offline, a quota, the network.
    UpstreamFailure,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// What the model reads, success or failure. No size cap.
    pub content: Vec<Content>,
    /// `None` is success. Never sent to the model; a wire with an error flag
    /// derives it from `error.is_some()`.
    pub error: Option<Failure>,
}

impl ToolResult {
    pub fn ok(text: impl Into<String>) -> Self {
        ToolResult {
            content: vec![Content::text(text)],
            error: None,
        }
    }

    pub fn failed(failure: Failure, text: impl Into<String>) -> Self {
        ToolResult {
            content: vec![Content::text(text)],
            error: Some(failure),
        }
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// Why `open` is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// The session's first run: `system` goes into the head.
    Fresh,
    /// Joins a session under way: `system` is discarded.
    Added,
    /// A new snapshot at run start (config change, upgrade): `system` is
    /// discarded.
    Reopened,
}

/// When a notice lands. Each tool-dependent mode names its own fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Delivery {
    /// After the next tool-result turn; if the model ends its turn first,
    /// after `end_turn`, and the run resumes.
    AfterToolOrRun,
    /// After the next tool-result turn; if the model ends its turn first,
    /// before the user's next message.
    AfterToolOrMessage,
    /// After `end_turn`; the run resumes for one more model call.
    AfterRun,
    /// Before the user's next message.
    OnMessage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "kebab-case")]
pub enum PostError {
    #[error("role-refused")]
    RoleRefused,
    #[error("too-large")]
    TooLarge,
    #[error("empty")]
    Empty,
    #[error("queue-full")]
    QueueFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "kebab-case")]
pub enum WithdrawError {
    #[error("already-delivered")]
    AlreadyDelivered,
    #[error("expired")]
    Expired,
}

/// A refusal from `validate` or `migrate`. Shown to the user, never to the
/// model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigError {
    /// A JSON Pointer into the config; `""` is the whole config.
    pub path: String,
    pub message: String,
}

impl ConfigError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        ConfigError {
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn whole(message: impl Into<String>) -> Self {
        ConfigError::new("", message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialized_names_are_the_wit_ones() {
        assert_eq!(
            serde_json::to_value(Failure::UpstreamFailure).unwrap(),
            json!("upstream-failure")
        );
        assert_eq!(
            serde_json::to_value(Delivery::AfterToolOrMessage).unwrap(),
            json!("after-tool-or-message")
        );
        assert_eq!(
            serde_json::to_value(Message::user("hi")).unwrap(),
            json!({ "role": "user", "content": [{ "type": "text", "text": "hi" }] })
        );
    }
}
