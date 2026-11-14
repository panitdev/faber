//! A fake daemon served by a fake host: events arrive in order, handles reach
//! their own sessions and store, and a replaced handle answers `gone`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plugin::Json;
use plugin::daemon::{
    BINDING_CAP, BindingHandle, Daemon, Event, Events, MemoryStore, SendError, SessionError,
    SessionRef, SharedStore, Store, check_send, valid_key,
};
use plugin::{Message, Plugin};
use serde_json::json;

/// The host's side of one binding: its key table, a gone flag, a store.
struct FakeBinding {
    id: String,
    config: Json,
    gone: AtomicBool,
    keys: Mutex<BTreeMap<String, String>>,
    sent: Mutex<Vec<(String, String)>>,
    store: Arc<MemoryStore>,
}

impl FakeBinding {
    fn new(id: &str, config: Json, store: Arc<MemoryStore>) -> Arc<Self> {
        Arc::new(FakeBinding {
            id: id.to_owned(),
            config,
            gone: AtomicBool::new(false),
            keys: Mutex::new(BTreeMap::new()),
            sent: Mutex::new(Vec::new()),
            store,
        })
    }
}

#[async_trait]
impl BindingHandle for FakeBinding {
    fn id(&self) -> &str {
        &self.id
    }
    fn config(&self) -> &Json {
        &self.config
    }
    async fn open(&self, key: &str, _title: Option<&str>) -> Result<SessionRef, SessionError> {
        if self.gone.load(Ordering::SeqCst) {
            return Err(SessionError::Gone);
        }
        if !valid_key(key) {
            return Err(SessionError::InvalidKey);
        }
        let mut keys = self.keys.lock().unwrap();
        let next = format!("{}-s{}", self.id, keys.len());
        let created = !keys.contains_key(key);
        let id = keys.entry(key.to_owned()).or_insert(next).clone();
        Ok(SessionRef { id, created })
    }
    async fn send(
        &self,
        session: &str,
        message: Message,
        dedupe: Option<&str>,
    ) -> Result<(), SendError> {
        if self.gone.load(Ordering::SeqCst) {
            return Err(SendError::Gone);
        }
        check_send(&message, dedupe)?;
        if !self.keys.lock().unwrap().values().any(|id| id == session) {
            return Err(SendError::NotOwned);
        }
        self.sent
            .lock()
            .unwrap()
            .push((session.to_owned(), message.text()));
        Ok(())
    }
    async fn archive(&self, _session: &str) -> Result<(), SessionError> {
        Ok(())
    }
    fn storage(&self) -> SharedStore {
        self.store.clone()
    }
}

/// Opens one session per binding, says hello, and checkpoints what it saw.
#[derive(Default)]
struct Echo {
    log: Mutex<Vec<String>>,
}

#[async_trait]
impl Daemon for Echo {
    async fn serve(&self, mut events: Events) -> Result<(), String> {
        let mut live: BTreeMap<String, Arc<dyn BindingHandle>> = BTreeMap::new();
        while let Some(event) = events.next().await {
            self.log.lock().unwrap().push(format!("{event:?}"));
            match event {
                Event::Added(handle) | Event::Changed(handle) => {
                    let session = handle
                        .open("thread:1", Some("hello"))
                        .await
                        .map_err(|e| e.to_string())?;
                    let channel = handle.config()["channel"]
                        .as_str()
                        .unwrap_or("?")
                        .to_owned();
                    handle
                        .send(
                            &session.id,
                            Message::user(format!("hi {channel}")),
                            Some("m1"),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    handle
                        .storage()
                        .put("seen", channel.into_bytes(), None)
                        .await
                        .map_err(|e| e.to_string())?;
                    live.insert(handle.id().to_owned(), handle);
                }
                Event::Removed(id) => {
                    live.remove(&id);
                }
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn a_daemon_serves_bindings_until_its_events_close() {
    let daemon = Arc::new(Echo::default());
    let (sender, events) = Events::channel(8);
    let serving = tokio::spawn({
        let daemon = Arc::clone(&daemon);
        async move { daemon.serve(events).await }
    });

    let store_a = MemoryStore::new(BINDING_CAP);
    let a = FakeBinding::new("a", json!({ "channel": "general" }), store_a.clone());
    let b = FakeBinding::new(
        "b",
        json!({ "channel": "ops" }),
        MemoryStore::new(BINDING_CAP),
    );
    sender.send(Event::Added(a.clone())).await.unwrap();
    sender.send(Event::Added(b.clone())).await.unwrap();

    // A config change: a new handle with the same id.
    let a2 = FakeBinding::new("a", json!({ "channel": "random" }), store_a.clone());
    sender.send(Event::Changed(a2.clone())).await.unwrap();
    sender.send(Event::Removed("b".into())).await.unwrap();

    drop(sender);
    serving.await.unwrap().unwrap();
    // The host retires the replaced handle; it answers `gone` from then on.
    a.gone.store(true, Ordering::SeqCst);

    assert_eq!(
        *daemon.log.lock().unwrap(),
        [
            "Added(\"a\")",
            "Added(\"b\")",
            "Changed(\"a\")",
            "Removed(\"b\")"
        ]
    );
    assert_eq!(
        *a.sent.lock().unwrap(),
        [("a-s0".into(), "hi general".into())]
    );
    assert_eq!(*b.sent.lock().unwrap(), [("b-s0".into(), "hi ops".into())]);
    assert_eq!(
        *a2.sent.lock().unwrap(),
        [("a-s0".into(), "hi random".into())]
    );
    // Both handles of `a` share its one store.
    assert_eq!(store_a.get("seen").await.unwrap().value, b"random");
    assert_eq!(a.open("thread:1", None).await, Err(SessionError::Gone));
    assert_eq!(
        a2.send("b-s0", Message::user("x"), None).await,
        Err(SendError::NotOwned)
    );
}

#[test]
fn v1_plugins_have_no_daemon() {
    struct Bare(plugin::Manifest);
    #[async_trait]
    impl Plugin for Bare {
        fn manifest(&self) -> &plugin::Manifest {
            &self.0
        }
        async fn open(
            &self,
            _: &plugin::SessionCtx,
            _: plugin::Phase,
            _: &Json,
        ) -> Result<Vec<plugin::Content>, String> {
            Ok(Vec::new())
        }
        async fn handle_tool(
            &self,
            _: &plugin::SessionCtx,
            _: &Json,
            _: plugin::ToolCall,
        ) -> plugin::ToolResult {
            plugin::ToolResult::ok("")
        }
        fn validate(&self, config: Json) -> Result<Json, Vec<plugin::ConfigError>> {
            Ok(config)
        }
        fn migrate(&self, config: Json, _: u32) -> Result<Json, Vec<plugin::ConfigError>> {
            Ok(config)
        }
        fn on_changed(&self, _: &Json, _: &Json) -> Vec<Message> {
            Vec::new()
        }
        fn on_removed(&self, _: &Json) -> Vec<Message> {
            Vec::new()
        }
    }
    let manifest = serde_json::from_value(json!({
        "id": "bare", "name": "bare", "version": "1.0.0", "description": "",
        "tools": [], "config": { "version": 1, "schema": { "type": "object" }, "default": {} }
    }))
    .unwrap();
    assert!(Bare(manifest).daemon().is_none());
}

mod registration {
    use super::*;
    use plugin::daemon::SharedDaemon;
    use plugin::registry::RegisterError;
    use plugin::{Manifest, Registry};

    struct Daemonish {
        manifest: Manifest,
        daemon: Option<SharedDaemon>,
    }

    #[async_trait]
    impl Plugin for Daemonish {
        fn manifest(&self) -> &Manifest {
            &self.manifest
        }
        fn daemon(&self) -> Option<SharedDaemon> {
            self.daemon.clone()
        }
        async fn open(
            &self,
            _: &plugin::SessionCtx,
            _: plugin::Phase,
            _: &Json,
        ) -> Result<Vec<plugin::Content>, String> {
            Ok(Vec::new())
        }
        async fn handle_tool(
            &self,
            _: &plugin::SessionCtx,
            _: &Json,
            _: plugin::ToolCall,
        ) -> plugin::ToolResult {
            plugin::ToolResult::ok("")
        }
        fn validate(&self, config: Json) -> Result<Json, Vec<plugin::ConfigError>> {
            Ok(config)
        }
        fn migrate(&self, config: Json, _: u32) -> Result<Json, Vec<plugin::ConfigError>> {
            Ok(config)
        }
        fn on_changed(&self, _: &Json, _: &Json) -> Vec<Message> {
            Vec::new()
        }
        fn on_removed(&self, _: &Json) -> Vec<Message> {
            Vec::new()
        }
    }

    fn manifest(capabilities: Json) -> Manifest {
        serde_json::from_value(json!({
            "id": "discord", "name": "Discord", "version": "1.0.0", "description": "",
            "capabilities": capabilities,
            "config": { "version": 1, "schema": { "type": "object" }, "default": {} }
        }))
        .unwrap()
    }

    fn register(capabilities: Json, daemon: bool) -> Result<(), RegisterError> {
        let daemon: Option<SharedDaemon> = daemon.then(|| Arc::new(Echo::default()) as _);
        Registry::new().register(
            Arc::new(Daemonish {
                manifest: manifest(capabilities),
                daemon,
            }),
            false,
        )
    }

    #[test]
    fn a_daemon_and_its_capability_come_together() {
        register(json!({ "daemon": {}, "storage": {} }), true).unwrap();
        register(json!({ "storage": {} }), false).unwrap();
        assert!(matches!(
            register(json!({ "daemon": {} }), false),
            Err(RegisterError::DaemonMissing(_))
        ));
        assert!(matches!(
            register(json!({}), true),
            Err(RegisterError::DaemonUndeclared(_))
        ));
    }

    #[test]
    fn websocket_without_daemon_is_refused_at_registration() {
        assert!(matches!(
            register(
                json!({ "websocket": { "hosts": ["gateway.discord.gg"] } }),
                false
            ),
            Err(RegisterError::Manifest(_))
        ));
        register(
            json!({ "daemon": {}, "websocket": { "hosts": ["*.discord.gg"] } }),
            true,
        )
        .unwrap();
    }
}
