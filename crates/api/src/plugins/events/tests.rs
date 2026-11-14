use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use diesel::sql_types::{BigInt, Jsonb, Text, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use plugin::events::{LAGGED, SessionEvent};
use plugin::{
    ConfigError, Content, Json, Manifest, Message, Notices, Phase, Plugin, SessionCtx, ToolCall,
    ToolResult,
};
use serde_json::json;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::{Limits, Listeners, Source};
use crate::plugins::daemon::fixture::Fixture;
use crate::run::StreamEvent;

struct FakeSource {
    sender: broadcast::Sender<StreamEvent>,
    running: AtomicBool,
}

impl FakeSource {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(FakeSource {
            sender: broadcast::channel(capacity).0,
            running: AtomicBool::new(true),
        })
    }

    fn publish(&self, run: Uuid, seq: i64, kind: &str) {
        let _ = self.sender.send(StreamEvent {
            run_id: run,
            seq,
            kind: kind.to_owned(),
            payload: json!({ "seq": seq }),
        });
    }

    fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

impl Source for FakeSource {
    fn subscribe(&self, _session: Uuid) -> broadcast::Receiver<StreamEvent> {
        self.sender.subscribe()
    }

    fn running(&self, _session: Uuid) -> bool {
        AtomicBool::load(&self.running, Ordering::SeqCst)
    }
}

/// Records what it is told, as `(kind, seq, config)`.
struct Recorder {
    manifest: Manifest,
    seen: Mutex<Vec<(String, i64, Json)>>,
    calls: AtomicUsize,
    /// Per call.
    delay: Duration,
    panics: bool,
    /// Sleeps this long on its first call only.
    stuck: Option<Duration>,
}

impl Recorder {
    fn new() -> Recorder {
        Recorder {
            manifest: serde_json::from_value(json!({
                "id": "listener", "name": "Listener", "version": "1.0.0", "description": "",
                "capabilities": { "events": {} },
                "config": { "version": 1, "schema": { "type": "object" }, "default": {} }
            }))
            .unwrap(),
            seen: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            panics: false,
            stuck: None,
        }
    }

    fn seen(&self) -> Vec<(String, i64)> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|(kind, seq, _)| (kind.clone(), *seq))
            .collect()
    }

    fn seqs(&self) -> Vec<i64> {
        self.seen()
            .into_iter()
            .filter(|(kind, _)| kind != LAGGED)
            .map(|(_, seq)| seq)
            .collect()
    }
}

#[async_trait]
impl Plugin for Recorder {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    async fn open(&self, _: &SessionCtx, _: Phase, _: &Json) -> Result<Vec<Content>, String> {
        Ok(Vec::new())
    }
    async fn handle_tool(&self, _: &SessionCtx, _: &Json, _: ToolCall) -> ToolResult {
        ToolResult::ok("")
    }
    async fn on_event(&self, _: &SessionCtx, config: &Json, event: SessionEvent) {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        if let (true, Some(stuck)) = (first, self.stuck) {
            tokio::time::sleep(stuck).await;
            return;
        }
        if self.panics {
            panic!("asked to");
        }
        tokio::time::sleep(self.delay).await;
        self.seen
            .lock()
            .unwrap()
            .push((event.kind, event.seq, config.clone()));
    }
    fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>> {
        Ok(config)
    }
    fn migrate(&self, config: Json, _: u32) -> Result<Json, Vec<ConfigError>> {
        Ok(config)
    }
    fn on_changed(&self, _: &Json, _: &Json) -> Vec<Message> {
        Vec::new()
    }
    fn on_removed(&self, _: &Json) -> Vec<Message> {
        Vec::new()
    }
}

fn limits() -> Limits {
    Limits {
        call: Duration::from_millis(300),
        idle: Duration::from_millis(50),
        flush: Duration::ZERO,
        failures: 3,
        page: 3,
    }
}

fn listener(plugin: &Arc<Recorder>, config: Json) -> plugin::Listener {
    plugin::Listener {
        plugin: Arc::clone(plugin) as Arc<dyn Plugin>,
        binding: "listener".to_owned(),
        order: 1,
        config,
    }
}

/// A thread and a run in the fixture's session.
async fn run_of(fixture: &Fixture) -> Uuid {
    let mut conn = fixture.pool.get().await.unwrap();
    let thread = Uuid::now_v7();
    diesel::sql_query(
        "INSERT INTO thread (id, session_id, next_seq, created_at) VALUES ($1, $2, 0, 0)",
    )
    .bind::<SqlUuid, _>(thread)
    .bind::<SqlUuid, _>(fixture.sessions[0])
    .execute(&mut conn)
    .await
    .unwrap();
    let run = Uuid::now_v7();
    diesel::sql_query("INSERT INTO run (id, thread_id, created_at) VALUES ($1, $2, 0)")
        .bind::<SqlUuid, _>(run)
        .bind::<SqlUuid, _>(thread)
        .execute(&mut conn)
        .await
        .unwrap();
    run
}

async fn persist(fixture: &Fixture, run: Uuid, seq: i64, kind: &str) {
    let mut conn = fixture.pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO transcript (id, run_id, seq, kind, payload, created_at) \
         VALUES ($1, $2, $3, $4, $5, 0)",
    )
    .bind::<SqlUuid, _>(Uuid::now_v7())
    .bind::<SqlUuid, _>(run)
    .bind::<BigInt, _>(seq)
    .bind::<Text, _>(kind)
    .bind::<Jsonb, _>(json!({ "seq": seq }))
    .execute(&mut conn)
    .await
    .unwrap();
}

async fn cursor(fixture: &Fixture) -> Option<(Uuid, i64)> {
    let mut conn = fixture.pool.get().await.unwrap();
    diesel::sql_query(
        "SELECT run_id, seq FROM binding_event_cursor \
         WHERE session_id = $1 AND plugin_type = 'listener'",
    )
    .bind::<SqlUuid, _>(fixture.sessions[0])
    .get_result::<super::rows::Cursor>(&mut conn)
    .await
    .ok()
    .map(|row| (row.run_id, row.seq))
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
#[ignore = "needs the database: set DATABASE_URL"]
async fn events_arrive_live_in_order_and_the_cursor_follows() {
    let fixture = Fixture::new(1).await;
    let session = fixture.sessions[0];
    let run = run_of(&fixture).await;
    // The user's turn is persisted and broadcast before the run starts its
    // plugins, as `launch_run` does: the catch-up is what gives it.
    persist(&fixture, run, 0, "input").await;
    let source = FakeSource::new(64);
    let listeners = Listeners::new(fixture.pool.clone(), limits());
    let plugin = Arc::new(Recorder::new());
    listeners.attach(
        source.clone(),
        session,
        run,
        listener(&plugin, json!({})),
        Notices::new(),
    );
    until("the input", || plugin.seqs() == [0]).await;

    // Live, while the run goes on: deltas are never persisted. A persisted
    // event the catch-up gave is not given twice.
    source.publish(run, 0, "input");
    source.publish(run, 1, "block_delta");
    source.publish(run, 2, "block_delta");
    until("the deltas", || plugin.seqs() == [0, 1, 2]).await;
    assert!(listeners.delivering(session, "listener"));

    persist(&fixture, run, 3, "run_end").await;
    source.publish(run, 3, "run_end");
    source.stop();
    until("the task to end", || {
        !listeners.delivering(session, "listener")
    })
    .await;
    assert_eq!(
        plugin.seen(),
        [
            ("input".to_owned(), 0),
            ("block_delta".to_owned(), 1),
            ("block_delta".to_owned(), 2),
            ("run_end".to_owned(), 3)
        ]
    );
    assert_eq!(cursor(&fixture).await, Some((run, 3)));
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "needs the database: set DATABASE_URL"]
async fn a_plugin_that_falls_behind_is_told_and_given_what_was_persisted() {
    let fixture = Fixture::new(1).await;
    let session = fixture.sessions[0];
    let run = run_of(&fixture).await;
    let source = FakeSource::new(4);
    let listeners = Listeners::new(fixture.pool.clone(), limits());
    let plugin = Arc::new(Recorder {
        delay: Duration::from_millis(20),
        ..Recorder::new()
    });
    listeners.attach(
        source.clone(),
        session,
        run,
        listener(&plugin, json!({})),
        Notices::new(),
    );
    for seq in 0..20 {
        persist(&fixture, run, seq, "message").await;
        source.publish(run, seq, "message");
    }
    source.stop();
    until("the task to end", || {
        !listeners.delivering(session, "listener")
    })
    .await;
    assert!(plugin.seen().iter().any(|(kind, _)| kind == LAGGED));
    // Every persisted event, once, in order.
    assert_eq!(plugin.seqs(), (0..20).collect::<Vec<_>>());
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "needs the database: set DATABASE_URL"]
async fn a_new_task_resumes_past_the_cursor() {
    let fixture = Fixture::new(1).await;
    let session = fixture.sessions[0];
    let earlier = run_of(&fixture).await;
    for seq in 0..3 {
        persist(&fixture, earlier, seq, "message").await;
    }
    // A run the last task never saw, as after a restart.
    let missed = run_of(&fixture).await;
    for seq in 0..5 {
        persist(&fixture, missed, seq, "message").await;
    }
    let mut conn = fixture.pool.get().await.unwrap();
    diesel::sql_query(
        "INSERT INTO binding_event_cursor (session_id, plugin_type, run_id, seq) \
         VALUES ($1, 'listener', $2, 1)",
    )
    .bind::<SqlUuid, _>(session)
    .bind::<SqlUuid, _>(earlier)
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);

    let source = FakeSource::new(64);
    source.stop();
    let listeners = Listeners::new(fixture.pool.clone(), limits());
    let plugin = Arc::new(Recorder::new());
    let current = run_of(&fixture).await;
    listeners.attach(
        source.clone(),
        session,
        current,
        listener(&plugin, json!({})),
        Notices::new(),
    );
    until("the task to end", || {
        !listeners.delivering(session, "listener")
    })
    .await;
    // Past the cursor, across runs, read three at a time.
    assert_eq!(plugin.seqs(), [2, 0, 1, 2, 3, 4]);
    assert_eq!(cursor(&fixture).await, Some((missed, 4)));
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "needs the database: set DATABASE_URL"]
async fn a_stuck_call_is_abandoned_and_a_failing_plugin_cut_off() {
    let fixture = Fixture::new(1).await;
    let session = fixture.sessions[0];
    let run = run_of(&fixture).await;
    let listeners = Listeners::new(fixture.pool.clone(), limits());

    let source = FakeSource::new(64);
    let stuck = Arc::new(Recorder {
        stuck: Some(Duration::from_secs(60)),
        ..Recorder::new()
    });
    listeners.attach(
        source.clone(),
        session,
        run,
        listener(&stuck, json!({})),
        Notices::new(),
    );
    source.publish(run, 0, "block_delta");
    source.publish(run, 1, "block_delta");
    until("the call after the stuck one", || stuck.seqs() == [1]).await;
    source.stop();
    until("the task to end", || {
        !listeners.delivering(session, "listener")
    })
    .await;

    let source = FakeSource::new(64);
    let failing = Arc::new(Recorder {
        panics: true,
        ..Recorder::new()
    });
    let second = run_of(&fixture).await;
    listeners.attach(
        source.clone(),
        session,
        second,
        listener(&failing, json!({})),
        Notices::new(),
    );
    for seq in 0..6 {
        source.publish(second, seq, "block_delta");
    }
    source.stop();
    until("the task to end", || {
        !listeners.delivering(session, "listener")
    })
    .await;
    assert_eq!(AtomicUsize::load(&failing.calls, Ordering::SeqCst), 3);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "needs the database: set DATABASE_URL"]
async fn a_later_run_hands_the_running_task_its_snapshot() {
    let fixture = Fixture::new(1).await;
    let session = fixture.sessions[0];
    let first = run_of(&fixture).await;
    let source = FakeSource::new(64);
    let listeners = Listeners::new(fixture.pool.clone(), limits());
    let plugin = Arc::new(Recorder::new());
    listeners.attach(
        source.clone(),
        session,
        first,
        listener(&plugin, json!({ "n": 1 })),
        Notices::new(),
    );
    source.publish(first, 0, "block_delta");
    until("the first run's event", || plugin.seqs() == [0]).await;

    let second = run_of(&fixture).await;
    listeners.attach(
        source.clone(),
        session,
        second,
        listener(&plugin, json!({ "n": 2 })),
        Notices::new(),
    );
    source.publish(second, 0, "block_delta");
    until("the second run's event", || plugin.seqs() == [0, 0]).await;
    let configs: Vec<Json> = plugin
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, _, config)| config.clone())
        .collect();
    assert_eq!(configs, [json!({ "n": 1 }), json!({ "n": 2 })]);
    source.stop();
    until("the task to end", || {
        !listeners.delivering(session, "listener")
    })
    .await;
    fixture.cleanup().await;
}
