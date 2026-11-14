//! The Discord plugin against a scripted gateway (`wss`, the daemon tests'
//! CA) and a fake REST API on localhost.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use plugin::daemon::{
    BindingHandle, Daemon, Event, Events, MemoryStore, SendError, SessionError, SessionRef,
    SharedStore, Store,
};
use plugin::events::SessionEvent;
use plugin::manifest::WebsocketCapability;
use plugin::{Json, Message, Notices, Plugin, SessionCtx};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

use super::*;
use crate::plugins::daemon::websocket::{Limits, PluginSockets, Resolve};

const CA: &str = include_str!("../daemon/testdata/ca.crt");
const CERT: &str = include_str!("../daemon/testdata/gateway.test.crt");
const KEY: &str = include_str!("../daemon/testdata/gateway.test.key");

struct Pinned(SocketAddr);

#[async_trait]
impl Resolve for Pinned {
    async fn resolve(&self, _: &str, _: u16) -> std::io::Result<Vec<SocketAddr>> {
        Ok(vec![self.0])
    }
}

/// What the fake host saw a binding do.
#[derive(Default)]
struct Seen {
    opened: Vec<String>,
    sent: Vec<(String, String, Option<String>)>,
    archived: Vec<String>,
}

struct FakeBinding {
    config: Json,
    store: Arc<MemoryStore>,
    seen: Mutex<Seen>,
}

#[async_trait]
impl BindingHandle for FakeBinding {
    fn id(&self) -> &str {
        "p/discord"
    }
    fn config(&self) -> &Json {
        &self.config
    }
    async fn open(&self, key: &str, _: Option<&str>) -> Result<SessionRef, SessionError> {
        let mut seen = self.seen.lock().unwrap();
        let created = !seen.opened.iter().any(|k| k == key);
        seen.opened.push(key.to_owned());
        Ok(SessionRef {
            id: format!("session-{key}"),
            created,
        })
    }
    async fn send(
        &self,
        session: &str,
        message: Message,
        dedupe: Option<&str>,
    ) -> Result<(), SendError> {
        self.seen.lock().unwrap().sent.push((
            session.to_owned(),
            message.text(),
            dedupe.map(str::to_owned),
        ));
        Ok(())
    }
    async fn archive(&self, session: &str) -> Result<(), SessionError> {
        self.seen.lock().unwrap().archived.push(session.to_owned());
        Ok(())
    }
    fn storage(&self) -> SharedStore {
        self.store.clone()
    }
}

fn dispatch(t: &str, s: u64, d: Value) -> String {
    json!({ "op": 0, "t": t, "s": s, "d": d }).to_string()
}

fn message(id: &str, channel: &str, author: &str, content: &str) -> Value {
    json!({
        "id": id, "channel_id": channel, "content": content,
        "author": { "id": format!("u-{author}"), "username": author },
    })
}

/// A gateway that plays one script per connection and reports every frame
/// the client sends.
async fn gateway(
    scripts: Vec<Vec<String>>,
) -> (
    SocketAddr,
    Arc<rustls::ClientConfig>,
    mpsc::UnboundedReceiver<(usize, Value)>,
) {
    use rustls_pki_types::pem::PemObject;
    let ca = rustls_pki_types::CertificateDer::from_pem_slice(CA.as_bytes()).unwrap();
    let cert = rustls_pki_types::CertificateDer::from_pem_slice(CERT.as_bytes()).unwrap();
    let key = rustls_pki_types::PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
    let (report, frames) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for (n, script) in scripts.into_iter().enumerate() {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let tls = acceptor.accept(tcp).await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tls).await.unwrap();
            ws.send(WsMessage::Text(
                json!({ "op": 10, "d": { "heartbeat_interval": 45000 } })
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            // The client's identify or resume, then the script.
            if let Some(Ok(WsMessage::Text(text))) = ws.next().await {
                let _ = report.send((n, serde_json::from_str(&text).unwrap()));
            }
            for line in script {
                ws.send(WsMessage::Text(line.into())).await.unwrap();
            }
            // Keep reading until the client goes, reporting what it sends.
            while let Some(Ok(frame)) = ws.next().await {
                if let WsMessage::Text(text) = frame {
                    let _ = report.send((n, serde_json::from_str(&text).unwrap()));
                }
            }
        }
    });
    (addr, Arc::new(client), frames)
}

/// The REST API: posts are kept, and typing calls counted per channel.
async fn rest() -> (
    String,
    Arc<Mutex<Vec<(String, Value)>>>,
    Arc<Mutex<Vec<String>>>,
) {
    use axum::{Json as Body, Router, extract::Path, routing::post};
    let posted = Arc::new(Mutex::new(Vec::new()));
    let typed = Arc::new(Mutex::new(Vec::new()));
    let keep = Arc::clone(&posted);
    let count = Arc::clone(&typed);
    let app = Router::new()
        .route(
            "/channels/{id}/typing",
            post(move |Path(id): Path<String>| {
                let count = Arc::clone(&count);
                async move {
                    count.lock().unwrap().push(id);
                    axum::http::StatusCode::NO_CONTENT
                }
            }),
        )
        .route(
            "/channels/{id}/messages",
            post(move |Path(id): Path<String>, Body(body): Body<Value>| {
                let keep = Arc::clone(&keep);
                async move {
                    keep.lock().unwrap().push((id, body));
                    Body(json!({}))
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), posted, typed)
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    for _ in 0..300 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn messages_become_sessions_and_a_restart_resumes_without_duplicates() {
    let first = vec![
        dispatch(
            "READY",
            1,
            json!({
                "session_id": "sess-1",
                "resume_gateway_url": "wss://gateway.test",
                "user": { "id": "bot" },
            }),
        ),
        dispatch("MESSAGE_CREATE", 2, message("m1", "c1", "ana", "hello")),
        // The bot's own message; one in a thread and one in another channel,
        // neither listened to; a member joining, which is no one's words.
        dispatch(
            "MESSAGE_CREATE",
            3,
            json!({
                "id": "m2", "channel_id": "c1", "content": "an echo",
                "author": { "id": "bot", "username": "faber", "bot": true },
            }),
        ),
        dispatch(
            "MESSAGE_CREATE",
            4,
            message("m3", "t1", "eve", "in a thread"),
        ),
        dispatch("MESSAGE_CREATE", 5, message("m5", "x9", "eve", "elsewhere")),
        dispatch(
            "MESSAGE_CREATE",
            6,
            json!({
                "id": "m6", "channel_id": "c1", "type": 7, "content": "",
                "author": { "id": "u-cy", "username": "cy" },
            }),
        ),
        dispatch(
            "MESSAGE_CREATE",
            7,
            message("m4", "c1", "bo", "in the channel"),
        ),
        json!({ "op": 7, "d": null }).to_string(),
    ];
    // After the reconnect, Discord replays m1.
    let second = vec![dispatch(
        "MESSAGE_CREATE",
        8,
        message("m1", "c1", "ana", "hello"),
    )];
    let (addr, tls, mut frames) = gateway(vec![first, second]).await;

    let sockets = PluginSockets::with(
        Some(WebsocketCapability {
            hosts: vec!["gateway.test".into()],
        }),
        Limits::default(),
        tls,
        Arc::new(Pinned(addr)),
    );
    let daemon = Arc::new(gateway::Gateway::with(
        sockets,
        "wss://gateway.test/?v=10&encoding=json",
        Duration::from_millis(10),
    ));
    let binding = Arc::new(FakeBinding {
        config: json!({ "token": "tok", "channel_id": "c1" }),
        store: MemoryStore::new(1 << 20),
        seen: Mutex::default(),
    });

    let (sender, events) = Events::channel(8);
    let serving = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move { daemon.serve(events).await }
    });
    sender.send(Event::Added(binding.clone())).await.unwrap();

    let (n, identify) = frames.recv().await.unwrap();
    assert_eq!((n, identify["op"].as_u64()), (0, Some(2)));
    assert_eq!(identify["d"]["token"], "tok");

    // The second connection resumes the first's session from its last seq.
    let (n, resume) = loop {
        let (n, frame) = frames.recv().await.unwrap();
        if n == 1 {
            break (n, frame);
        }
    };
    assert_eq!(n, 1);
    assert_eq!(resume["op"], 6);
    assert_eq!(resume["d"]["session_id"], "sess-1");
    assert_eq!(resume["d"]["seq"], 7);

    eventually("the replay arrives", || {
        binding.seen.lock().unwrap().sent.len() == 3
    })
    .await;
    {
        let seen = binding.seen.lock().unwrap();
        assert_eq!(
            seen.sent,
            [
                (
                    "session-channel:c1".to_owned(),
                    "ana (Discord user u-ana) wrote:\nhello".to_owned(),
                    Some("m1".to_owned()),
                ),
                (
                    "session-channel:c1".to_owned(),
                    "bo (Discord user u-bo) wrote:\nin the channel".to_owned(),
                    Some("m4".to_owned()),
                ),
                // The replay carries the same dedupe id, so the host drops it.
                (
                    "session-channel:c1".to_owned(),
                    "ana (Discord user u-ana) wrote:\nhello".to_owned(),
                    Some("m1".to_owned()),
                ),
            ]
        );
        // One session for the channel, found again for every message.
        assert!(seen.opened.iter().all(|key| key == "channel:c1"));
        assert!(seen.archived.is_empty());
    }

    // Shutdown: the checkpoint holds the session and the last seq.
    drop(sender);
    serving.await.unwrap().unwrap();
    let checkpoint: gateway::Checkpoint =
        serde_json::from_slice(&binding.store.get("gateway").await.unwrap().value).unwrap();
    assert_eq!(checkpoint.session_id.as_deref(), Some("sess-1"));
    assert_eq!(checkpoint.seq, Some(8));
    assert_eq!(checkpoint.v, 1);
}

/// `key_of` from a table, and one store for every session.
struct Keys(Vec<(String, String)>, SharedStore);

impl Keys {
    fn new(keys: Vec<(String, String)>) -> Keys {
        Keys(keys, MemoryStore::new(1 << 20))
    }
}

#[async_trait]
impl plugin::daemon::DaemonHost for Keys {
    async fn key_of(&self, ctx: &SessionCtx) -> Option<String> {
        self.0
            .iter()
            .find(|(session, _)| session == ctx.session_id())
            .map(|(_, key)| key.clone())
    }
    async fn store_of(&self, _: &SessionCtx) -> SharedStore {
        Arc::clone(&self.1)
    }
}

struct Idle;

#[async_trait]
impl Daemon for Idle {
    async fn serve(&self, _: Events) -> Result<(), String> {
        Ok(())
    }
}

fn event(run: Uuid, seq: i64, kind: &str, payload: Value) -> SessionEvent {
    SessionEvent {
        run_id: run.to_string(),
        seq,
        kind: kind.to_owned(),
        payload,
    }
}

#[tokio::test]
async fn the_bot_types_while_a_run_goes_on_and_posts_only_the_agents_text() {
    let (base, posted, typed) = rest().await;
    let discord = Discord::with(
        Arc::new(Idle),
        Arc::new(Keys::new(vec![("s1".into(), "channel:c1".into())])),
        Api::with_base(base),
    )
    .typing_every(Duration::from_millis(20));
    let config = json!({ "token": "tok", "channel_id": "c1" });
    let ctx = SessionCtx::new("s1", ID, 0, Notices::new());
    let run = Uuid::now_v7();
    let typings = || typed.lock().unwrap().len();

    // The user's turn starts the indicator, and it is renewed.
    discord
        .on_event(
            &ctx,
            &config,
            event(run, 0, "input", json!({ "role": "user" })),
        )
        .await;
    eventually("the indicator to be renewed", || typings() >= 2).await;
    assert!(typed.lock().unwrap().iter().all(|channel| channel == "c1"));

    // Reasoning and a tool call are not posted.
    let working = json!({ "role": "assistant", "content": [
        { "type": "thinking", "thinking": "let me think" },
        { "type": "tool_use", "id": "t", "name": "web__search", "input": {} },
    ]});
    discord
        .on_event(&ctx, &config, event(run, 5, "message", working))
        .await;
    // A long answer, in Discord-sized parts.
    let long = "word ".repeat(500);
    let answer = json!({ "role": "assistant", "content": [
        { "type": "thinking", "thinking": "secret" },
        { "type": "text", "text": long },
    ]});
    discord
        .on_event(&ctx, &config, event(run, 9, "message", answer.clone()))
        .await;
    discord
        .on_event(&ctx, &config, event(run, 10, "run_end", Value::Null))
        .await;
    {
        let posted = posted.lock().unwrap();
        assert_eq!(posted.len(), 2);
        assert!(posted.iter().all(|(channel, _)| channel == "c1"));
        assert_eq!(posted[0].1["allowed_mentions"], json!({ "parse": [] }));
        let text: String = posted
            .iter()
            .map(|(_, body)| body["content"].as_str().unwrap())
            .collect();
        assert!(!text.contains("secret") && !text.contains("let me think"));
    }

    // Stopped at the run's end.
    let stopped = typings();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(typings(), stopped);

    // Delivered again, as after a restart: not posted twice.
    discord
        .on_event(&ctx, &config, event(run, 9, "message", answer))
        .await;
    discord
        .on_event(&ctx, &config, event(run, 10, "run_end", Value::Null))
        .await;
    assert_eq!(posted.lock().unwrap().len(), 2);

    // A session Discord did not start is left alone.
    let owners = SessionCtx::new("s2", ID, 0, Notices::new());
    let other = Uuid::now_v7();
    let said = json!({ "role": "assistant", "content": [{ "type": "text", "text": "hi" }] });
    discord
        .on_event(&owners, &config, event(other, 0, "input", json!({})))
        .await;
    discord
        .on_event(&owners, &config, event(other, 1, "message", said))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(posted.lock().unwrap().len(), 2);
    assert_eq!(typings(), stopped);
    assert!(
        discord
            .open(&owners, plugin::Phase::Fresh, &config)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !discord
            .open(&ctx, plugin::Phase::Fresh, &config)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn config_is_checked_and_trimmed_and_the_manifest_registers() {
    let discord = Discord::with(Arc::new(Idle), Arc::new(Keys::new(Vec::new())), Api::new());
    assert_eq!(
        discord.validate(json!({ "token": " tok ", "channel_id": " 123 " })),
        Ok(json!({ "token": "tok", "channel_id": "123" }))
    );
    assert_eq!(discord.validate(json!({})), Ok(json!({})));
    assert!(
        discord
            .validate(json!({ "channel_id": "general" }))
            .is_err()
    );
    assert!(discord.validate(json!({ "token": "a b" })).is_err());
    assert!(discord.validate(json!({ "guild": "1" })).is_err());
    assert_eq!(Settings::of(&json!({ "token": "tok" })), None);

    let mut registry = plugin::Registry::new();
    registry.register(Arc::new(discord), false).unwrap();
}
