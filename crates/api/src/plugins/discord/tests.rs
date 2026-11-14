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
use plugin::manifest::WebsocketCapability;
use plugin::{Json, Message, Notices, Plugin, SessionCtx, ToolCall};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

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

/// The REST API: every channel's parent is `other`, and posts are kept.
async fn rest() -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
    use axum::{
        Json as Body, Router,
        extract::Path,
        routing::{get, post},
    };
    let posted = Arc::new(Mutex::new(Vec::new()));
    let keep = Arc::clone(&posted);
    let app =
        Router::new()
            .route(
                "/channels/{id}",
                get(|Path(id): Path<String>| async move {
                    Body(json!({ "id": id, "parent_id": "other" }))
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
    (format!("http://{addr}"), posted)
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
        dispatch(
            "GUILD_CREATE",
            2,
            json!({ "threads": [{ "id": "t1", "parent_id": "c1" }] }),
        ),
        dispatch(
            "MESSAGE_CREATE",
            3,
            message("m1", "t1", "ana", "hello from the thread"),
        ),
        // The bot's own message, and one in a channel that is not ours.
        dispatch(
            "MESSAGE_CREATE",
            4,
            json!({
                "id": "m2", "channel_id": "t1", "content": "an echo",
                "author": { "id": "bot", "username": "faber", "bot": true },
            }),
        ),
        dispatch("MESSAGE_CREATE", 5, message("m3", "x9", "eve", "elsewhere")),
        dispatch(
            "MESSAGE_CREATE",
            6,
            message("m4", "c1", "bo", "in the channel"),
        ),
        dispatch(
            "THREAD_UPDATE",
            7,
            json!({
                "id": "t1", "parent_id": "c1", "thread_metadata": { "archived": true },
            }),
        ),
        json!({ "op": 7, "d": null }).to_string(),
    ];
    // After the reconnect, Discord replays m1.
    let second = vec![dispatch(
        "MESSAGE_CREATE",
        8,
        message("m1", "t1", "ana", "hello from the thread"),
    )];
    let (addr, tls, mut frames) = gateway(vec![first, second]).await;
    let (base, _) = rest().await;

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
        Api::with_base(base),
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
                    "session-thread:t1".to_owned(),
                    "ana (Discord user u-ana) wrote:\nhello from the thread".to_owned(),
                    Some("m1".to_owned()),
                ),
                (
                    "session-channel:c1".to_owned(),
                    "bo (Discord user u-bo) wrote:\nin the channel".to_owned(),
                    Some("m4".to_owned()),
                ),
                // The replay carries the same dedupe id, so the host drops it.
                (
                    "session-thread:t1".to_owned(),
                    "ana (Discord user u-ana) wrote:\nhello from the thread".to_owned(),
                    Some("m1".to_owned()),
                ),
            ]
        );
        assert_eq!(seen.archived, ["session-thread:t1"]);
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

/// `key_of` from a table; `store_of` is not used by the plugin.
struct Keys(Vec<(String, String)>);

#[async_trait]
impl plugin::daemon::DaemonHost for Keys {
    async fn key_of(&self, ctx: &SessionCtx) -> Option<String> {
        self.0
            .iter()
            .find(|(session, _)| session == ctx.session_id())
            .map(|(_, key)| key.clone())
    }
    async fn store_of(&self, _: &SessionCtx) -> SharedStore {
        MemoryStore::new(0)
    }
}

struct Idle;

#[async_trait]
impl Daemon for Idle {
    async fn serve(&self, _: Events) -> Result<(), String> {
        Ok(())
    }
}

#[tokio::test]
async fn reply_posts_where_the_session_came_from_and_nowhere_else() {
    let (base, posted) = rest().await;
    let discord = Discord::with(
        Arc::new(Idle),
        Arc::new(Keys(vec![("s1".into(), "thread:t1".into())])),
        Api::with_base(base),
    );
    let config = json!({ "token": "tok", "channel_id": "c1" });
    let ctx = SessionCtx::new("s1", ID, 0, Notices::new());
    let call = |text: &str| ToolCall {
        id: "call".into(),
        name: "reply".into(),
        input: json!({ "text": text }),
    };

    let long = "word ".repeat(500);
    let result = discord.handle_tool(&ctx, &config, call(&long)).await;
    assert!(!result.is_error(), "{result:?}");
    assert_eq!(plugin::joined(&result.content), "Posted, as 2 messages.");
    {
        let posted = posted.lock().unwrap();
        assert_eq!(posted.len(), 2);
        assert!(posted.iter().all(|(channel, _)| channel == "t1"));
        assert_eq!(posted[0].1["allowed_mentions"], json!({ "parse": [] }));
    }

    // A session Discord did not start has nowhere to reply.
    let owners = SessionCtx::new("s2", ID, 0, Notices::new());
    let result = discord.handle_tool(&owners, &config, call("hi")).await;
    assert_eq!(result.error, Some(plugin::Failure::BadRequest));
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
    let discord = Discord::with(Arc::new(Idle), Arc::new(Keys(Vec::new())), Api::new());
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
