//! The few Discord REST calls the plugin makes, over a client that reads no
//! proxy or other ambient setting from this process.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

/// Where Discord's REST API is.
pub const API_BASE: &str = "https://discord.com/api/v10";

/// Characters of one Discord message.
pub const MESSAGE_CHARS: usize = 2000;

#[derive(Clone)]
pub struct Api {
    base: String,
    client: reqwest::Client,
}

/// Why a call failed, worded for whoever reads it: the model, for a reply.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Discord refused the bot's token or permissions ({0})")]
    Refused(u16),
    #[error("Discord is rate limiting the bot; try again in {0:.1} s")]
    RateLimited(f64),
    #[error("Discord answered {0}")]
    Status(u16),
    #[error("Discord could not be reached: {0}")]
    Network(String),
}

#[derive(Deserialize)]
struct Channel {
    parent_id: Option<String>,
}

#[derive(Deserialize)]
struct RateLimit {
    retry_after: f64,
}

impl Api {
    pub fn new() -> Self {
        Api::with_base(API_BASE)
    }

    /// Against another base URL: tests point it at a local server.
    pub fn with_base(base: impl Into<String>) -> Self {
        Api {
            base: base.into(),
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(20))
                .user_agent("Faber Discord plugin")
                .build()
                .expect("a plain HTTP client builds"),
        }
    }

    async fn check(response: reqwest::Response) -> Result<reqwest::Response, ApiError> {
        let status = response.status().as_u16();
        match status {
            200..=299 => Ok(response),
            401 | 403 => Err(ApiError::Refused(status)),
            429 => {
                let wait = response
                    .json::<RateLimit>()
                    .await
                    .map_or(1.0, |limit| limit.retry_after);
                Err(ApiError::RateLimited(wait))
            }
            _ => Err(ApiError::Status(status)),
        }
    }

    /// The parent of a thread; `None` for a channel that is not one.
    pub async fn parent_of(&self, token: &str, channel: &str) -> Result<Option<String>, ApiError> {
        let response = self
            .client
            .get(format!("{}/channels/{channel}", self.base))
            .header("Authorization", format!("Bot {token}"))
            .send()
            .await
            .map_err(|error| ApiError::Network(error.to_string()))?;
        let channel: Channel = Self::check(response)
            .await?
            .json()
            .await
            .map_err(|error| ApiError::Network(error.to_string()))?;
        Ok(channel.parent_id)
    }

    /// Posts `text` to `channel`, split into Discord-sized messages. Mentions
    /// are never parsed: a reply cannot ping `@everyone` or anyone else.
    pub async fn post(&self, token: &str, channel: &str, text: &str) -> Result<usize, ApiError> {
        let parts = split(text, MESSAGE_CHARS);
        for part in &parts {
            let response = self
                .client
                .post(format!("{}/channels/{channel}/messages", self.base))
                .header("Authorization", format!("Bot {token}"))
                .json(&json!({ "content": part, "allowed_mentions": { "parse": [] } }))
                .send()
                .await
                .map_err(|error| ApiError::Network(error.to_string()))?;
            Self::check(response).await?;
        }
        Ok(parts.len())
    }
}

/// `text` in pieces of at most `chars` characters, broken at a line end
/// where one is near.
pub fn split(text: &str, chars: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut rest: Vec<char> = text.chars().collect();
    while rest.len() > chars {
        let window = &rest[..chars];
        let cut = window
            .iter()
            .rposition(|c| *c == '\n')
            .filter(|at| *at >= chars / 2)
            .map_or(chars, |at| at + 1);
        parts.push(rest[..cut].iter().collect());
        rest.drain(..cut);
    }
    if !rest.is_empty() || parts.is_empty() {
        parts.push(rest.into_iter().collect());
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_splits_at_line_ends_within_the_limit() {
        assert_eq!(split("hi", 10), ["hi"]);
        let text = format!("{}\n{}", "a".repeat(8), "b".repeat(8));
        assert_eq!(
            split(&text, 10),
            [format!("{}\n", "a".repeat(8)), "b".repeat(8)]
        );
        let solid = "x".repeat(25);
        let parts = split(&solid, 10);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|part| part.chars().count() <= 10));
        assert_eq!(parts.concat(), solid);
    }
}
