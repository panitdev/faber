//! The Discord daemon: one gateway connection per binding, all in one
//! `serve` call.
//!
//! Each binding's connection identifies with its own bot token, or resumes
//! from the checkpoint in its store (session id, resume URL, sequence), so
//! a restarted daemon picks up where the last one stopped and Discord
//! replays what it missed. Replays are harmless: every message is sent with
//! its Discord id as the dedupe id.
//!
//! Every member message in the bound channel goes to the channel's one live
//! session, keyed `channel:<id>`: `open` finds it, or makes one when the last
//! was archived. Threads and other channels are not listened to. The sender's
//! name and id lead the text, so the agent can tell channel members apart and
//! from the owner. The agent's answers go back through the plugin's event
//! handler, not from here.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use plugin::Message;
use plugin::daemon::{
    Author, Close, Daemon, Envelope, Event, Events, Frame, ReplyTo, SendError, SharedBinding,
    SharedStore, Socket, SocketError, Websocket,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::Settings;

/// Where a fresh connection goes.
pub const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// What a resume URL is given, as Discord asks.
const QUERY: &str = "?v=10&encoding=json";
/// GUILDS, GUILD_MESSAGES and MESSAGE_CONTENT (privileged: the bot must
/// have it switched on in Discord's developer portal).
const INTENTS: u64 = (1 << 0) | (1 << 9) | (1 << 15);
/// The store key the checkpoint lives under.
const CHECKPOINT: &str = "gateway";
/// How often a changed checkpoint is written while connected.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(10);
/// Bytes of a member's text passed on; the rest is cut.
const TEXT_CAP: usize = 7 * 1024;

/// What a binding's store keeps across restarts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The layout of this record, for a later version to migrate.
    pub v: u32,
    pub session_id: Option<String>,
    pub resume_url: Option<String>,
    pub seq: Option<u64>,
}

/// The daemon, as the supervisor runs it.
pub struct Gateway {
    sockets: Arc<dyn Websocket>,
    url: String,
    /// Waits between reconnects, doubling from this to 60 times it.
    backoff: Duration,
}

impl Gateway {
    pub fn new(sockets: Arc<dyn Websocket>) -> Self {
        Gateway::with(sockets, GATEWAY_URL, Duration::from_secs(1))
    }

    pub fn with(sockets: Arc<dyn Websocket>, url: impl Into<String>, backoff: Duration) -> Self {
        Gateway {
            sockets,
            url: url.into(),
            backoff,
        }
    }
}

/// One binding's connection task, and how to stop it.
struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Running {
    async fn stop(self) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .is_err()
        {
            tracing::warn!("a Discord connection did not stop in time");
        }
    }
}

#[async_trait]
impl Daemon for Gateway {
    async fn serve(&self, mut events: Events) -> Result<(), String> {
        let mut running: HashMap<String, Running> = HashMap::new();
        while let Some(event) = events.next().await {
            match event {
                Event::Added(handle) | Event::Changed(handle) => {
                    if let Some(old) = running.remove(handle.id()) {
                        old.stop().await;
                    }
                    let Some(settings) = Settings::of(handle.config()) else {
                        tracing::info!(binding = %handle.id(), "Discord binding has no token or channel yet");
                        continue;
                    };
                    let (stop, stopped) = watch::channel(false);
                    let connection = Connection {
                        handle: Arc::clone(&handle),
                        store: handle.storage(),
                        settings,
                        sockets: Arc::clone(&self.sockets),
                        url: self.url.clone(),
                        backoff: self.backoff,
                        me: None,
                    };
                    let id = handle.id().to_owned();
                    let task = tokio::spawn(connection.run(stopped));
                    running.insert(id, Running { stop, task });
                }
                Event::Removed(id) => {
                    if let Some(old) = running.remove(&id) {
                        old.stop().await;
                    }
                }
            }
        }
        // Shutdown: every connection checkpoints and closes resumably.
        for (_, connection) in running.drain() {
            connection.stop().await;
        }
        Ok(())
    }
}

/// How one gateway session ended.
enum Ended {
    Stopped,
    /// Reconnect; resume when `resumable`.
    Reconnect {
        resumable: bool,
    },
    /// The token or intents are wrong: retrying cannot help.
    Fatal(String),
}

struct Connection {
    handle: SharedBinding,
    store: SharedStore,
    settings: Settings,
    sockets: Arc<dyn Websocket>,
    url: String,
    backoff: Duration,
    /// The bot's own user id, so its messages are skipped.
    me: Option<String>,
}

impl Connection {
    async fn run(mut self, mut stopped: watch::Receiver<bool>) {
        let mut checkpoint = self.load().await;
        let mut backoff = self.backoff;
        loop {
            if *stopped.borrow() {
                break;
            }
            let url = match (&checkpoint.session_id, &checkpoint.resume_url) {
                (Some(_), Some(resume)) => format!("{}/{QUERY}", resume.trim_end_matches('/')),
                _ => self.url.clone(),
            };
            let ended = match self.sockets.connect(self.handle.as_ref(), &url).await {
                Ok(socket) => {
                    let ended = self
                        .session(socket.as_ref(), &mut checkpoint, &mut stopped)
                        .await;
                    if !matches!(ended, Ended::Reconnect { resumable: false }) {
                        backoff = self.backoff;
                    }
                    ended
                }
                Err(SocketError::NotAllowed(reason)) => Ended::Fatal(reason),
                Err(error) => {
                    tracing::warn!(binding = %self.handle.id(), %error, "Discord gateway unreachable");
                    Ended::Reconnect { resumable: true }
                }
            };
            match ended {
                Ended::Stopped => break,
                Ended::Fatal(reason) => {
                    tracing::error!(binding = %self.handle.id(), %reason, "Discord binding stopped");
                    break;
                }
                Ended::Reconnect { resumable } => {
                    if !resumable {
                        checkpoint.session_id = None;
                        checkpoint.resume_url = None;
                        checkpoint.seq = None;
                    }
                    self.save(&checkpoint).await;
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = stopped.changed() => break,
                    }
                    backoff = (backoff * 2).min(self.backoff * 60);
                }
            }
        }
        self.save(&checkpoint).await;
    }

    async fn load(&self) -> Checkpoint {
        match self.store.get(CHECKPOINT).await {
            Ok(entry) => serde_json::from_slice(&entry.value).unwrap_or_default(),
            Err(_) => Checkpoint::default(),
        }
    }

    async fn save(&self, checkpoint: &Checkpoint) {
        let mut record = checkpoint.clone();
        record.v = 1;
        let bytes = serde_json::to_vec(&record).expect("a checkpoint serializes");
        if let Err(error) = self.store.put(CHECKPOINT, bytes, None).await {
            tracing::warn!(binding = %self.handle.id(), %error, "Discord checkpoint not saved");
        }
    }

    async fn send(&self, socket: &dyn Socket, payload: Value) -> Result<(), SocketError> {
        socket.send(Frame::Text(payload.to_string())).await
    }

    /// One connection, from Hello to its end.
    async fn session(
        &mut self,
        socket: &dyn Socket,
        checkpoint: &mut Checkpoint,
        stopped: &mut watch::Receiver<bool>,
    ) -> Ended {
        // Hello first: it says how often to heartbeat.
        let hello = tokio::select! {
            frame = socket.receive() => frame,
            _ = stopped.changed() => return Ended::Stopped,
        };
        let interval = match hello.map(parse) {
            Ok(Some(payload)) if payload.op == 10 => payload.d["heartbeat_interval"]
                .as_u64()
                .map_or(Duration::from_secs(41), Duration::from_millis),
            Ok(_) => return Ended::Reconnect { resumable: true },
            Err(error) => return closed(error),
        };

        let start = match (&checkpoint.session_id, checkpoint.seq) {
            (Some(session), Some(seq)) => json!({ "op": 6, "d": {
                "token": self.settings.token, "session_id": session, "seq": seq,
            }}),
            _ => json!({ "op": 2, "d": {
                "token": self.settings.token,
                "intents": INTENTS,
                "properties": { "os": "linux", "browser": "faber", "device": "faber" },
            }}),
        };
        if let Err(error) = self.send(socket, start).await {
            return closed(error);
        }

        let mut heartbeat = tokio::time::interval(interval);
        heartbeat.tick().await;
        let mut acked = true;
        let mut saving = tokio::time::interval(CHECKPOINT_EVERY);
        saving.tick().await;
        let mut saved = checkpoint.clone();
        loop {
            tokio::select! {
                _ = stopped.changed() => {
                    // Not 1000 or 1001, which would end the session: the next
                    // start resumes it.
                    socket.close(Close::new(4000, "restarting"));
                    return Ended::Stopped;
                }
                _ = heartbeat.tick() => {
                    if !acked {
                        // A zombie connection: no ack since the last beat.
                        socket.close(Close::new(4000, "no heartbeat ack"));
                        return Ended::Reconnect { resumable: true };
                    }
                    acked = false;
                    if let Err(error) = self.send(socket, json!({ "op": 1, "d": checkpoint.seq })).await {
                        return closed(error);
                    }
                }
                _ = saving.tick() => {
                    if *checkpoint != saved {
                        self.save(checkpoint).await;
                        saved = checkpoint.clone();
                    }
                }
                frame = socket.receive() => {
                    let payload = match frame.map(parse) {
                        Ok(Some(payload)) => payload,
                        Ok(None) => continue,
                        Err(error) => return closed(error),
                    };
                    match payload.op {
                        0 => {
                            if let Some(seq) = payload.s {
                                checkpoint.seq = Some(seq);
                            }
                            if let Err(ended) = self.dispatch(&payload, checkpoint).await {
                                return ended;
                            }
                        }
                        1 => {
                            if let Err(error) = self.send(socket, json!({ "op": 1, "d": checkpoint.seq })).await {
                                return closed(error);
                            }
                        }
                        7 => {
                            socket.close(Close::new(4000, "reconnect requested"));
                            return Ended::Reconnect { resumable: true };
                        }
                        9 => {
                            socket.close(Close::new(4000, "invalid session"));
                            return Ended::Reconnect { resumable: payload.d.as_bool() == Some(true) };
                        }
                        11 => acked = true,
                        _ => {}
                    }
                }
            }
        }
    }

    async fn dispatch(
        &mut self,
        payload: &Payload,
        checkpoint: &mut Checkpoint,
    ) -> Result<(), Ended> {
        let d = &payload.d;
        match payload.t.as_deref() {
            Some("READY") => {
                checkpoint.session_id = d["session_id"].as_str().map(str::to_owned);
                checkpoint.resume_url = d["resume_gateway_url"].as_str().map(str::to_owned);
                self.me = d["user"]["id"].as_str().map(str::to_owned);
                self.save(checkpoint).await;
            }
            Some("MESSAGE_CREATE") => self.message(d).await?,
            _ => {}
        }
        Ok(())
    }

    async fn message(&mut self, d: &Value) -> Result<(), Ended> {
        let author = &d["author"];
        if author["bot"].as_bool() == Some(true)
            || author["id"]
                .as_str()
                .is_some_and(|id| Some(id) == self.me.as_deref())
        {
            return Ok(());
        }
        // What members write (0) and their replies (19), not joins, pins and
        // the like.
        if !matches!(d["type"].as_u64(), None | Some(0) | Some(19)) {
            return Ok(());
        }
        let (Some(channel), Some(id)) = (d["channel_id"].as_str(), d["id"].as_str()) else {
            return Ok(());
        };
        if channel != self.settings.channel {
            return Ok(());
        }
        let key = format!("channel:{channel}");
        let Some(said) = said(d, self.me.as_deref()) else {
            return Ok(());
        };

        let session = match self.handle.open(&key, None).await {
            Ok(session) => session.id,
            Err(plugin::daemon::SessionError::Gone) => return Err(Ended::Stopped),
            Err(error) => {
                tracing::warn!(binding = %self.handle.id(), %key, %error, "Discord message dropped: no session");
                return Ok(());
            }
        };
        match self.handle.send(&session, said, Some(id)).await {
            Ok(()) => Ok(()),
            Err(SendError::Gone) => Err(Ended::Stopped),
            Err(error) => {
                tracing::warn!(binding = %self.handle.id(), %key, %error, "Discord message not accepted");
                Ok(())
            }
        }
    }
}

/// A member's message for the session: what they wrote, who they are, and
/// whose message it answers when it is a reply. `me` is the bot's own id.
fn said(d: &Value, me: Option<&str>) -> Option<Envelope> {
    // Plain text only, for now: attachments and embeds are not passed on.
    let mut content = d["content"].as_str().unwrap_or("").trim().to_owned();
    if content.is_empty() {
        return None;
    }
    if content.len() > TEXT_CAP {
        cut(&mut content, TEXT_CAP);
        content.push_str(" […]");
    }
    let mut envelope = Envelope::from(Message::user(content));
    envelope.from = member(&d["author"]);
    let replied = &d["referenced_message"]["author"];
    envelope.reply_to = match replied["id"].as_str() {
        Some(id) if Some(id) == me => Some(ReplyTo::Agent),
        Some(_) => member(replied).map(ReplyTo::Author),
        None => None,
    };
    Some(envelope)
}

/// A Discord user as an author: their id, the name the channel shows, and
/// their picture when they have set one.
fn member(user: &Value) -> Option<Author> {
    let id = user["id"].as_str().filter(|id| !id.is_empty())?;
    let mut name = user["global_name"]
        .as_str()
        .or_else(|| user["username"].as_str())
        .unwrap_or("someone")
        .to_owned();
    cut(&mut name, plugin::daemon::NAME_CAP);
    let mut author = Author::new(id, name);
    // The hash goes into a URL: only what Discord's hashes are made of.
    author.avatar = user["avatar"]
        .as_str()
        .filter(|hash| {
            !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .filter(|_| id.bytes().all(|b| b.is_ascii_digit()))
        .map(|hash| format!("https://cdn.discordapp.com/avatars/{id}/{hash}.png?size=64"));
    Some(author)
}

/// Cuts `text` to at most `cap` bytes, on a character boundary.
fn cut(text: &mut String, cap: usize) {
    let mut at = cap.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    text.truncate(at);
}

#[derive(Deserialize)]
struct Payload {
    op: u8,
    #[serde(default)]
    d: Value,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

fn parse(frame: Frame) -> Option<Payload> {
    match frame {
        Frame::Text(text) => serde_json::from_str(&text).ok(),
        Frame::Binary(bytes) => serde_json::from_slice(&bytes).ok(),
    }
}

/// What a socket error means for the gateway session.
fn closed(error: SocketError) -> Ended {
    match error {
        SocketError::Closed(Close { code, reason }) => match code {
            4004 => Ended::Fatal("Discord refused the bot token".into()),
            4010..=4014 => Ended::Fatal(format!("Discord refused the connection: {code} {reason}")),
            4007 | 4009 => Ended::Reconnect { resumable: false },
            _ => Ended::Reconnect { resumable: true },
        },
        SocketError::NotAllowed(reason) => Ended::Fatal(reason),
        SocketError::ConnectFailed(_) | SocketError::TooLarge => {
            Ended::Reconnect { resumable: true }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_is_the_author_and_long_text_is_cut() {
        let hello = said(
            &json!({
                "author": { "id": "42", "username": "ana", "global_name": "Ana", "avatar": "a_1f" },
                "content": " hello ",
            }),
            Some("1"),
        )
        .unwrap();
        assert_eq!(hello.message.text(), "hello");
        let ana = hello.from.unwrap();
        assert_eq!((ana.id.as_str(), ana.name.as_str()), ("42", "Ana"));
        assert_eq!(
            ana.avatar.as_deref(),
            Some("https://cdn.discordapp.com/avatars/42/a_1f.png?size=64")
        );
        assert_eq!(hello.reply_to, None);

        assert_eq!(said(&json!({ "author": {}, "content": "  " }), None), None);
        let long = said(
            &json!({ "author": {}, "content": "é".repeat(TEXT_CAP) }),
            None,
        )
        .unwrap();
        assert_eq!(long.from, None);
        assert!(long.message.text().len() < TEXT_CAP + 100);
        assert!(long.message.text().ends_with("[…]"));
        // A hash that is not one never reaches a URL.
        let odd = member(&json!({ "id": "42", "username": "ana", "avatar": "../x" })).unwrap();
        assert_eq!(odd.avatar, None);
    }

    #[test]
    fn a_reply_names_who_it_answers() {
        let reply = |to: Value| {
            said(
                &json!({
                    "type": 19,
                    "author": { "id": "42", "username": "ana" },
                    "content": "yes",
                    "referenced_message": to,
                }),
                Some("1"),
            )
            .unwrap()
            .reply_to
        };
        assert_eq!(
            reply(json!({ "author": { "id": "1", "username": "faber", "bot": true } })),
            Some(ReplyTo::Agent)
        );
        assert_eq!(
            reply(json!({ "author": { "id": "77", "username": "bo" } })),
            Some(ReplyTo::Author(Author::new("77", "bo")))
        );
        // The message it answered was deleted.
        assert_eq!(reply(Value::Null), None);
    }

    #[test]
    fn close_codes_decide_whether_to_resume() {
        let code = |code| closed(SocketError::Closed(Close::new(code, "")));
        assert!(matches!(code(4004), Ended::Fatal(_)));
        assert!(matches!(code(4014), Ended::Fatal(_)));
        assert!(matches!(code(4009), Ended::Reconnect { resumable: false }));
        assert!(matches!(code(1006), Ended::Reconnect { resumable: true }));
    }
}
