//! Discord as a built-in plugin: the first daemon.
//!
//! The owner binds it to a project with a bot token and a channel id. Its
//! daemon ([`gateway::Gateway`]) holds one gateway connection per binding
//! and turns members' messages into sessions of the project: one for the
//! channel, one per thread under it. The agent answers with `discord__reply`,
//! which finds where to post from the key its session was opened under.
//!
//! The bot token sits in the binding's config, like any plugin's settings,
//! and every bound project's token is in the one daemon's memory.

pub mod api;
pub mod gateway;

use std::sync::Arc;

use async_trait::async_trait;
use plugin::daemon::{DaemonHost, SharedDaemon, Websocket};
use plugin::manifest::{
    Capabilities, ConfigSpec, DaemonCapability, HttpCapability, StorageCapability, ToolDefinition,
    WebsocketCapability,
};
use plugin::{
    ConfigError, Content, Failure, Json, Manifest, Message, Phase, Plugin, SessionCtx, ToolCall,
    ToolResult,
};
use serde_json::json;

use self::api::Api;

pub const ID: &str = "discord";

/// The hosts the gateway lives on: the first connection, and every resume
/// URL Discord hands out.
pub fn websocket_capability() -> WebsocketCapability {
    WebsocketCapability {
        hosts: vec!["gateway.discord.gg".into(), "*.discord.gg".into()],
    }
}

/// A binding's settings, once both are given.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub token: String,
    /// The channel whose messages, and whose threads' messages, become
    /// sessions.
    pub channel: String,
}

impl Settings {
    /// `None` until the owner has set both.
    pub fn of(config: &Json) -> Option<Settings> {
        let token = config["token"].as_str()?.trim();
        let channel = config["channel_id"].as_str()?.trim();
        (!token.is_empty() && !channel.is_empty()).then(|| Settings {
            token: token.to_owned(),
            channel: channel.to_owned(),
        })
    }
}

pub struct Discord {
    manifest: Manifest,
    daemon: SharedDaemon,
    host: Arc<dyn DaemonHost>,
    api: Api,
}

impl Discord {
    pub fn new(sockets: Arc<dyn Websocket>, host: Arc<dyn DaemonHost>) -> Self {
        let api = Api::new();
        let daemon = Arc::new(gateway::Gateway::new(sockets, api.clone()));
        Discord::with(daemon, host, api)
    }

    /// With another daemon and API: tests point both at local servers.
    pub fn with(daemon: SharedDaemon, host: Arc<dyn DaemonHost>, api: Api) -> Self {
        Discord {
            manifest: manifest(),
            daemon,
            host,
            api,
        }
    }
}

fn manifest() -> Manifest {
    Manifest {
        id: ID.to_owned(),
        name: "Discord".to_owned(),
        version: "0.1.0".to_owned(),
        description: "Turns a Discord channel and its threads into sessions of this project; \
                      the agent replies in the thread."
            .to_owned(),
        capabilities: Capabilities {
            http: Some(HttpCapability {
                hosts: vec!["discord.com".into()],
            }),
            daemon: Some(DaemonCapability {}),
            websocket: Some(websocket_capability()),
            storage: Some(StorageCapability {}),
        },
        tools: vec![ToolDefinition {
            name: "reply".to_owned(),
            description: "Post a message in the Discord channel or thread this session came \
                          from. Members see it as the bot. Long text is split into several \
                          messages; mentions are not pinged."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "minLength": 1, "description": "What to post." }
                },
                "required": ["text"],
                "additionalProperties": false
            }),
        }],
        config: ConfigSpec {
            version: 1,
            schema: json!({
                "type": "object",
                "properties": {
                    "token": {
                        "type": "string",
                        "description": "The bot's token, from Discord's developer portal. \
                                        The bot needs the Message Content intent."
                    },
                    "channel_id": {
                        "type": "string",
                        "description": "The channel to listen in. Its threads are included."
                    }
                },
                "additionalProperties": false
            }),
            default: json!({}),
        },
    }
}

/// The channel a session key names: `channel:<id>` or `thread:<id>`.
fn channel_of(key: &str) -> Option<&str> {
    key.strip_prefix("thread:")
        .or_else(|| key.strip_prefix("channel:"))
}

#[async_trait]
impl Plugin for Discord {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn daemon(&self) -> Option<SharedDaemon> {
        Some(Arc::clone(&self.daemon))
    }

    async fn open(
        &self,
        ctx: &SessionCtx,
        _phase: Phase,
        _config: &Json,
    ) -> Result<Vec<Content>, String> {
        // Only a session the channel started is about the channel.
        let Some(key) = self.host.key_of(ctx).await else {
            return Ok(Vec::new());
        };
        let place = if key.starts_with("thread:") {
            "a Discord thread"
        } else {
            "a Discord channel"
        };
        Ok(vec![Content::text(format!(
            "This session is {place}. Each user message is one or more members' messages, \
             each led by the member's name and Discord user id; they are channel members, \
             not necessarily the project's owner. They see only what you post with \
             `discord__reply`, never your other output."
        ))])
    }

    async fn handle_tool(&self, ctx: &SessionCtx, config: &Json, call: ToolCall) -> ToolResult {
        if call.name != "reply" {
            return ToolResult::failed(Failure::HandlerFailure, format!("no tool `{}`", call.name));
        }
        let Some(text) = call.input["text"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            return ToolResult::failed(Failure::BadRequest, "`text` is empty");
        };
        let Some(key) = self.host.key_of(ctx).await else {
            return ToolResult::failed(
                Failure::BadRequest,
                "This session was not started from Discord, so there is nowhere to reply.",
            );
        };
        let Some(channel) = channel_of(&key) else {
            return ToolResult::failed(
                Failure::HandlerFailure,
                format!("unknown session key `{key}`"),
            );
        };
        let Some(settings) = Settings::of(config) else {
            return ToolResult::failed(
                Failure::UpstreamFailure,
                "The Discord binding has no bot token; the project's owner must set one.",
            );
        };
        match self.api.post(&settings.token, channel, text).await {
            Ok(1) => ToolResult::ok("Posted."),
            Ok(parts) => ToolResult::ok(format!("Posted, as {parts} messages.")),
            Err(error) => ToolResult::failed(Failure::UpstreamFailure, error.to_string()),
        }
    }

    fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>> {
        let Some(object) = config.as_object() else {
            return Err(vec![ConfigError::whole("expected an object")]);
        };
        let mut errors = Vec::new();
        let mut normalized = serde_json::Map::new();
        for (name, value) in object {
            match (name.as_str(), value.as_str()) {
                ("token", Some(token)) => {
                    let token = token.trim();
                    if token.contains(char::is_whitespace) {
                        errors.push(ConfigError::new("/token", "a bot token has no spaces"));
                    } else if !token.is_empty() {
                        normalized.insert(name.clone(), json!(token));
                    }
                }
                ("channel_id", Some(channel)) => {
                    let channel = channel.trim();
                    if !channel.chars().all(|c| c.is_ascii_digit()) {
                        errors.push(ConfigError::new(
                            "/channel_id",
                            "a channel id is a number: right-click the channel and Copy Channel ID",
                        ));
                    } else if !channel.is_empty() {
                        normalized.insert(name.clone(), json!(channel));
                    }
                }
                ("token" | "channel_id", None) => {
                    errors.push(ConfigError::new(format!("/{name}"), "expected a string"))
                }
                _ => errors.push(ConfigError::new(
                    format!("/{name}"),
                    "not a Discord setting",
                )),
            }
        }
        if errors.is_empty() {
            Ok(Json::Object(normalized))
        } else {
            Err(errors)
        }
    }

    fn migrate(&self, config: Json, _from: u32) -> Result<Json, Vec<ConfigError>> {
        Ok(config)
    }

    fn on_changed(&self, _old: &Json, _new: &Json) -> Vec<Message> {
        Vec::new()
    }

    fn on_removed(&self, _config: &Json) -> Vec<Message> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests;
