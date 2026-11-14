//! Discord as a built-in plugin: the first daemon.
//!
//! The owner binds it to a project with a bot token and a channel id. Its
//! daemon ([`gateway::Gateway`]) holds one gateway connection per binding
//! and puts members' messages into the channel's one live session, opening
//! a new one when the last was archived. The agent answers as it would
//! anyone: the plugin listens to the session's events, shows the bot as
//! typing while a run goes on, and posts the text of each assistant message
//! to the channel. Reasoning and tool calls stay in Faber.
//!
//! The bot token sits in the binding's config, like any plugin's settings,
//! and every bound project's token is in the one daemon's memory.

pub mod api;
pub mod gateway;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use plugin::daemon::{DaemonHost, SharedDaemon, Websocket};
use plugin::events::SessionEvent;
use plugin::manifest::{
    Capabilities, ConfigSpec, DaemonCapability, EventsCapability, HttpCapability,
    StorageCapability, WebsocketCapability,
};
use plugin::{
    ConfigError, Content, Failure, Json, Manifest, Message, Phase, Plugin, SessionCtx, ToolCall,
    ToolResult,
};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use uuid::Uuid;

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

/// How often the typing indicator is renewed; Discord's lasts 10 seconds.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// Sessions whose key is remembered before the memory is cleared.
const KEYS_CAP: usize = 10_000;

pub struct Discord {
    manifest: Manifest,
    daemon: SharedDaemon,
    host: Arc<dyn DaemonHost>,
    api: Api,
    /// Each session's key, as `key_of` answered: it never changes, and every
    /// streamed event would otherwise ask.
    keys: Mutex<HashMap<String, Option<String>>>,
    /// The typing indicator of each session a run is going on in.
    typing: Mutex<HashMap<String, JoinHandle<()>>>,
    typing_every: Duration,
}

impl Discord {
    pub fn new(sockets: Arc<dyn Websocket>, host: Arc<dyn DaemonHost>) -> Self {
        let daemon = Arc::new(gateway::Gateway::new(sockets));
        Discord::with(daemon, host, Api::new())
    }

    /// With another daemon and API: tests point both at local servers.
    pub fn with(daemon: SharedDaemon, host: Arc<dyn DaemonHost>, api: Api) -> Self {
        Discord {
            manifest: manifest(),
            daemon,
            host,
            api,
            keys: Mutex::new(HashMap::new()),
            typing: Mutex::new(HashMap::new()),
            typing_every: TYPING_EVERY,
        }
    }

    /// With the typing indicator renewed this often: tests wait less.
    #[cfg(test)]
    fn typing_every(mut self, every: Duration) -> Self {
        self.typing_every = every;
        self
    }

    async fn key_of(&self, ctx: &SessionCtx) -> Option<String> {
        let session = ctx.session_id();
        if let Some(key) = self
            .keys
            .lock()
            .expect("session keys poisoned")
            .get(session)
        {
            return key.clone();
        }
        let key = self.host.key_of(ctx).await;
        let mut keys = self.keys.lock().expect("session keys poisoned");
        if keys.len() >= KEYS_CAP {
            keys.clear();
        }
        keys.insert(session.to_owned(), key.clone());
        key
    }

    /// Shows the bot as typing in `channel` until [`Discord::stop_typing`].
    fn start_typing(&self, session: &str, token: &str, channel: &str) {
        let mut typing = self.typing.lock().expect("typing poisoned");
        if typing.get(session).is_some_and(|task| !task.is_finished()) {
            return;
        }
        let (api, token, channel) = (self.api.clone(), token.to_owned(), channel.to_owned());
        let every = self.typing_every;
        let task = tokio::spawn(async move {
            loop {
                if let Err(error) = api.typing(&token, &channel).await {
                    tracing::debug!(%channel, %error, "Discord typing indicator not shown");
                }
                tokio::time::sleep(every).await;
            }
        });
        typing.insert(session.to_owned(), task);
    }

    fn stop_typing(&self, session: &str) {
        if let Some(task) = self.typing.lock().expect("typing poisoned").remove(session) {
            task.abort();
        }
    }

    /// Posts the text of an assistant message, unless it was posted already:
    /// events are delivered again after a restart.
    async fn answer(&self, ctx: &SessionCtx, token: &str, channel: &str, event: &SessionEvent) {
        let text = text_of(&event.payload);
        if text.trim().is_empty() {
            return;
        }
        let Ok(run) = event.run_id.parse::<Uuid>() else {
            return;
        };
        let store = self.host.store_of(ctx).await;
        let key = format!("posted:{}", ctx.session_id());
        let last: Option<(Uuid, i64)> = store
            .get(&key)
            .await
            .ok()
            .and_then(|entry| serde_json::from_slice::<Value>(&entry.value).ok())
            .and_then(|value| Some((value["run"].as_str()?.parse().ok()?, value["seq"].as_i64()?)));
        if last.is_some_and(|last| (run, event.seq) <= last) {
            return;
        }
        if let Err(error) = self.api.post(token, channel, text.trim()).await {
            tracing::warn!(%channel, %error, "the agent's answer was not posted to Discord");
            return;
        }
        let record = json!({ "run": run, "seq": event.seq })
            .to_string()
            .into_bytes();
        if let Err(error) = store.put(&key, record, None).await {
            tracing::warn!(%channel, %error, "could not record what was posted to Discord");
        }
    }
}

/// The text blocks of a message event, joined: not its reasoning, tool calls
/// or anything else.
fn text_of(message: &Value) -> String {
    message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn manifest() -> Manifest {
    Manifest {
        id: ID.to_owned(),
        name: "Discord".to_owned(),
        version: "0.1.0".to_owned(),
        description: "Turns a Discord channel into a thread of this project; the agent's \
                      answers are posted back to the channel."
            .to_owned(),
        capabilities: Capabilities {
            http: Some(HttpCapability {
                hosts: vec!["discord.com".into()],
            }),
            daemon: Some(DaemonCapability {}),
            websocket: Some(websocket_capability()),
            storage: Some(StorageCapability {}),
            events: Some(EventsCapability {}),
        },
        tools: Vec::new(),
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
                        "description": "The channel to listen in."
                    }
                },
                "additionalProperties": false
            }),
            default: json!({}),
        },
    }
}

/// The channel a session key names: `channel:<id>`, or `thread:<id>` for a
/// session an earlier version opened for a thread.
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
        if channel_of(&key).is_none() {
            return Ok(Vec::new());
        }
        Ok(vec![Content::text(
            "This session is a Discord channel. Messages with source `discord` are from its \
             members, by Discord user id and name. Everything you write as text is posted to \
             the channel as the bot; your reasoning and tool calls are not.",
        )])
    }

    async fn handle_tool(&self, _ctx: &SessionCtx, _config: &Json, call: ToolCall) -> ToolResult {
        ToolResult::failed(Failure::HandlerFailure, format!("no tool `{}`", call.name))
    }

    async fn on_event(&self, ctx: &SessionCtx, config: &Json, event: SessionEvent) {
        // Only the sessions this binding's channel opened answer to it.
        let Some(key) = self.key_of(ctx).await else {
            return;
        };
        let (Some(channel), Some(settings)) = (channel_of(&key), Settings::of(config)) else {
            return;
        };
        let session = ctx.session_id();
        match event.kind.as_str() {
            "run_end" | "run_error" | "run_interrupted" => self.stop_typing(session),
            plugin::events::LAGGED => {}
            "message" if event.payload["role"] == "assistant" => {
                if text_of(&event.payload).trim().is_empty() {
                    // Reasoning or tool calls only: still working.
                    self.start_typing(session, &settings.token, channel);
                    return;
                }
                // Discord ends the indicator when the bot posts; the run's
                // next event starts it again.
                self.stop_typing(session);
                self.answer(ctx, &settings.token, channel, &event).await;
            }
            // The user's turn, reasoning and text as they stream, tool calls
            // and results: a run is going on.
            _ => self.start_typing(session, &settings.token, channel),
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
