//! Telling plugins a session's events, live.
//!
//! A binding whose plugin requests the `events` capability gets every event
//! of the sessions it is active in through `on_event`: the same stream the
//! UI's `GET /api/sessions/{id}/stream` carries, deltas included. One task
//! per (session, binding) does it, started by the first run that needs it and
//! kept while runs of the session go on.
//!
//! The run never waits on a plugin. Each task holds its own subscription to
//! the session's broadcast and calls the plugin one event at a time, so a
//! slow plugin delays only itself; if it falls far enough behind that the
//! broadcast drops events for it, it is told so with a `lagged` event and
//! then given the persisted events it missed. Where a task got to is kept in
//! `binding_event_cursor`, so the next task, after an idle session wakes or
//! the server restarts, starts from there. Persisted events (whole messages,
//! tool results, run markers) therefore arrive at least once; deltas are
//! best effort.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use diesel::sql_types::{BigInt, Uuid as SqlUuid};
use diesel_async::RunQueryDsl;
use futures::FutureExt;
use plugin::events::{LAGGED, SessionEvent};
use plugin::{Json, Notices, Plugin, SessionCtx};
use serde_json::json;
use tokio::sync::broadcast::{self, error::RecvError};
use uuid::Uuid;

use crate::db::DbPool;
use crate::run::{KIND_RUN_END, KIND_RUN_ERROR, KIND_RUN_INTERRUPTED, RunRegistry, StreamEvent};

/// Where a session's live events come from, and whether more are coming.
pub trait Source: Send + Sync {
    fn subscribe(&self, session: Uuid) -> broadcast::Receiver<StreamEvent>;
    /// Whether a run of `session` is under way, so its events may still come.
    fn running(&self, session: Uuid) -> bool;
}

/// The server's own runs.
pub struct Runs(pub RunRegistry);

impl Source for Runs {
    fn subscribe(&self, session: Uuid) -> broadcast::Receiver<StreamEvent> {
        crate::run::subscribe(&self.0, session)
    }

    fn running(&self, session: Uuid) -> bool {
        crate::run::is_running(&self.0, session)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// How long one `on_event` may take before it is abandoned.
    pub call: Duration,
    /// How often an idle task checks whether its session's runs are over.
    pub idle: Duration,
    /// How often the cursor is written while events flow; run ends are
    /// written at once.
    pub flush: Duration,
    /// Failed calls in a row after which the binding is told nothing more
    /// until the session's next run.
    pub failures: u32,
    /// Persisted events read per query when catching up.
    pub page: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            call: Duration::from_secs(30),
            idle: Duration::from_secs(5),
            flush: Duration::from_secs(2),
            failures: 10,
            page: 500,
        }
    }
}

/// Every delivery task, by (session, binding).
pub struct Listeners {
    db: DbPool,
    limits: Limits,
    tasks: Mutex<HashMap<(Uuid, String), Arc<Delivery>>>,
}

/// One task's target: what it calls, with what. A later run of the session
/// replaces it, since its snapshot may carry a new config.
struct Delivery {
    target: Mutex<Target>,
}

struct Target {
    plugin: Arc<dyn Plugin>,
    ctx: SessionCtx,
    config: Json,
    /// Failed calls in a row.
    failures: u32,
}

impl Listeners {
    pub fn new(db: DbPool, limits: Limits) -> Arc<Self> {
        Arc::new(Listeners {
            db,
            limits,
            tasks: Mutex::new(HashMap::new()),
        })
    }

    /// Starts telling `listener` the events of `session`, from where it was
    /// last told them, or from the start of `run` when it never was. With a
    /// task already doing so, hands it this run's snapshot instead.
    pub fn attach(
        self: &Arc<Self>,
        source: Arc<dyn Source>,
        session: Uuid,
        run: Uuid,
        listener: plugin::Listener,
        notices: Arc<Notices>,
    ) {
        let target = Target {
            ctx: SessionCtx::new(
                session.to_string(),
                listener.binding.clone(),
                listener.order,
                notices,
            ),
            plugin: listener.plugin,
            config: listener.config,
            failures: 0,
        };
        let key = (session, listener.binding);
        let mut tasks = self.tasks.lock().expect("event listeners poisoned");
        if let Some(delivery) = tasks.get(&key) {
            *delivery.target.lock().expect("event target poisoned") = target;
            return;
        }
        let delivery = Arc::new(Delivery {
            target: Mutex::new(target),
        });
        tasks.insert(key.clone(), Arc::clone(&delivery));
        // Before the catch-up reads the transcript, so nothing published in
        // between is missed.
        let receiver = source.subscribe(session);
        drop(tasks);
        let listeners = Arc::clone(self);
        tokio::spawn(async move {
            Task {
                listeners: Arc::clone(&listeners),
                source,
                session,
                binding: key.1.clone(),
                delivery: Arc::clone(&delivery),
                receiver,
                at: (run, i64::MIN),
                delivered: HashMap::new(),
                dirty: false,
                flushed: Instant::now(),
            }
            .run()
            .await;
        });
    }

    /// Whether a task is telling `binding` the events of `session`.
    #[cfg(test)]
    fn delivering(&self, session: Uuid, binding: &str) -> bool {
        self.tasks
            .lock()
            .unwrap()
            .contains_key(&(session, binding.to_owned()))
    }
}

struct Task {
    listeners: Arc<Listeners>,
    source: Arc<dyn Source>,
    session: Uuid,
    binding: String,
    delivery: Arc<Delivery>,
    receiver: broadcast::Receiver<StreamEvent>,
    /// The last event given to the plugin, or the bound before the first.
    at: (Uuid, i64),
    /// The highest seq given, per run, so a live event the catch-up already
    /// gave is not given twice.
    delivered: HashMap<Uuid, i64>,
    dirty: bool,
    flushed: Instant,
}

/// Runs remembered in `delivered`; older ones' live events are long gone.
const REMEMBERED_RUNS: usize = 16;

impl Task {
    async fn run(mut self) {
        if let Some(cursor) = self.cursor().await {
            self.at = cursor;
        }
        self.catch_up().await;
        loop {
            match tokio::time::timeout(self.listeners.limits.idle, self.receiver.recv()).await {
                Ok(Ok(event)) => {
                    let ends = [KIND_RUN_END, KIND_RUN_ERROR, KIND_RUN_INTERRUPTED]
                        .contains(&event.kind.as_str());
                    self.give(event).await;
                    if ends {
                        self.flush().await;
                    }
                }
                Ok(Err(RecvError::Lagged(dropped))) => {
                    tracing::warn!(session = %self.session, binding = %self.binding, dropped, "plugin fell behind the session's events");
                    let (run, seq) = self.at;
                    self.call(SessionEvent {
                        run_id: run.to_string(),
                        seq,
                        kind: LAGGED.to_owned(),
                        payload: json!({ "dropped": dropped }),
                    })
                    .await;
                    self.catch_up().await;
                }
                Ok(Err(RecvError::Closed)) => {
                    self.flush().await;
                    self.forget();
                    return;
                }
                Err(_idle) => {
                    if self.source.running(self.session) {
                        continue;
                    }
                    self.flush().await;
                    // Checked again under the lock a new run's `attach`
                    // takes: a run that starts now either finds this task
                    // and hands it its snapshot, or finds none and starts its
                    // own from the cursor just written.
                    let mut tasks = self
                        .listeners
                        .tasks
                        .lock()
                        .expect("event listeners poisoned");
                    if self.source.running(self.session) || !self.receiver.is_empty() {
                        continue;
                    }
                    self.remove(&mut tasks);
                    return;
                }
            }
            if self.dirty && self.flushed.elapsed() >= self.listeners.limits.flush {
                self.flush().await;
            }
        }
    }

    /// Gives a live event, unless the catch-up already gave it.
    async fn give(&mut self, event: StreamEvent) {
        if self
            .delivered
            .get(&event.run_id)
            .is_some_and(|seq| event.seq <= *seq)
        {
            return;
        }
        self.mark(event.run_id, event.seq);
        self.call(SessionEvent {
            run_id: event.run_id.to_string(),
            seq: event.seq,
            kind: event.kind,
            payload: event.payload,
        })
        .await;
    }

    fn mark(&mut self, run: Uuid, seq: i64) {
        let highest = self.delivered.entry(run).or_insert(seq);
        *highest = (*highest).max(seq);
        if self.delivered.len() > REMEMBERED_RUNS
            && let Some(oldest) = self.delivered.keys().min().copied()
        {
            self.delivered.remove(&oldest);
        }
        self.at = self.at.max((run, seq));
        self.dirty = true;
    }

    /// Gives the persisted events past `at`.
    async fn catch_up(&mut self) {
        loop {
            let page = match self.persisted().await {
                Ok(page) => page,
                Err(error) => {
                    tracing::error!(session = %self.session, binding = %self.binding, %error, "could not read the session's events for a plugin");
                    return;
                }
            };
            let full = page.len() as i64 >= self.listeners.limits.page;
            for row in page {
                self.mark(row.run_id, row.seq);
                self.call(SessionEvent {
                    run_id: row.run_id.to_string(),
                    seq: row.seq,
                    kind: row.kind,
                    payload: row.payload,
                })
                .await;
            }
            if !full {
                return;
            }
        }
    }

    async fn persisted(&self) -> Result<Vec<rows::Event>, String> {
        let mut conn = self.listeners.db.get().await.map_err(|e| e.to_string())?;
        diesel::sql_query(
            "SELECT t.run_id, t.seq, t.kind, t.payload \
             FROM transcript t \
             JOIN run r ON r.id = t.run_id \
             JOIN thread th ON th.id = r.thread_id \
             WHERE th.session_id = $1 AND (t.run_id, t.seq) > ($2, $3) \
             ORDER BY t.run_id, t.seq \
             LIMIT $4",
        )
        .bind::<SqlUuid, _>(self.session)
        .bind::<SqlUuid, _>(self.at.0)
        .bind::<BigInt, _>(self.at.1)
        .bind::<BigInt, _>(self.listeners.limits.page)
        .load(&mut conn)
        .await
        .map_err(|e| e.to_string())
    }

    /// One call, bounded in time; a panic or a timeout counts against the
    /// binding.
    async fn call(&self, event: SessionEvent) {
        let limits = self.listeners.limits;
        let (plugin, ctx, config) = {
            let target = self.delivery.target.lock().expect("event target poisoned");
            if target.failures >= limits.failures {
                return;
            }
            (
                Arc::clone(&target.plugin),
                target.ctx.clone(),
                target.config.clone(),
            )
        };
        let kind = event.kind.clone();
        let called =
            std::panic::AssertUnwindSafe(plugin.on_event(&ctx, &config, event)).catch_unwind();
        let failed = match tokio::time::timeout(limits.call, called).await {
            Ok(Ok(())) => None,
            Ok(Err(_panic)) => Some("panicked"),
            Err(_) => Some("took too long and was abandoned"),
        };
        let mut target = self.delivery.target.lock().expect("event target poisoned");
        match failed {
            None => target.failures = 0,
            Some(how) => {
                target.failures += 1;
                tracing::warn!(session = %self.session, binding = %self.binding, %kind, failures = target.failures, "plugin event handler {how}");
                if target.failures == limits.failures {
                    tracing::error!(session = %self.session, binding = %self.binding, "plugin is told no more of this session's events until its next run");
                }
            }
        }
    }

    async fn cursor(&self) -> Option<(Uuid, i64)> {
        let mut conn = self.listeners.db.get().await.ok()?;
        let row: Option<rows::Cursor> = diesel::sql_query(
            "SELECT run_id, seq FROM binding_event_cursor \
             WHERE session_id = $1 AND plugin_type = $2",
        )
        .bind::<SqlUuid, _>(self.session)
        .bind::<diesel::sql_types::Text, _>(&self.binding)
        .get_result(&mut conn)
        .await
        .ok();
        row.map(|row| (row.run_id, row.seq))
    }

    async fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let written = async {
            let mut conn = self.listeners.db.get().await.map_err(|e| e.to_string())?;
            diesel::sql_query(
                "INSERT INTO binding_event_cursor (session_id, plugin_type, run_id, seq) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (session_id, plugin_type) \
                 DO UPDATE SET run_id = $3, seq = $4, updated_at = now()",
            )
            .bind::<SqlUuid, _>(self.session)
            .bind::<diesel::sql_types::Text, _>(&self.binding)
            .bind::<SqlUuid, _>(self.at.0)
            .bind::<BigInt, _>(self.at.1)
            .execute(&mut conn)
            .await
            .map_err(|e| e.to_string())
        }
        .await;
        match written {
            Ok(_) => {
                self.dirty = false;
                self.flushed = Instant::now();
            }
            Err(error) => {
                tracing::error!(session = %self.session, binding = %self.binding, %error, "could not record how far a plugin was told the session's events")
            }
        }
    }

    fn forget(&self) {
        let mut tasks = self
            .listeners
            .tasks
            .lock()
            .expect("event listeners poisoned");
        self.remove(&mut tasks);
    }

    /// Removes this task's entry, and only this task's.
    fn remove(&self, tasks: &mut HashMap<(Uuid, String), Arc<Delivery>>) {
        let key = (self.session, self.binding.clone());
        if tasks
            .get(&key)
            .is_some_and(|delivery| Arc::ptr_eq(delivery, &self.delivery))
        {
            tasks.remove(&key);
        }
    }
}

#[allow(clippy::redundant_field_names)]
mod rows {
    use diesel::QueryableByName;
    use diesel::sql_types::{BigInt, Jsonb, Text, Uuid as SqlUuid};
    use uuid::Uuid;

    #[derive(QueryableByName)]
    pub struct Event {
        #[diesel(sql_type = SqlUuid)]
        pub run_id: Uuid,
        #[diesel(sql_type = BigInt)]
        pub seq: i64,
        #[diesel(sql_type = Text)]
        pub kind: String,
        #[diesel(sql_type = Jsonb)]
        pub payload: serde_json::Value,
    }

    #[derive(QueryableByName)]
    pub struct Cursor {
        #[diesel(sql_type = SqlUuid)]
        pub run_id: Uuid,
        #[diesel(sql_type = BigInt)]
        pub seq: i64,
    }
}

#[cfg(test)]
mod tests;
