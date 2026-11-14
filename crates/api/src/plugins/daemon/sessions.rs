//! The session calls a binding makes: `open` by key, `send` with a dedupe id,
//! `archive`, and `key_of` for its tool handlers.
//!
//! A session a binding opens is an ordinary session of the project, in the
//! owner's personal workspace, so the owner sees it and every session route
//! reaches it. Its runs use the owner's model and credentials: the model the
//! owner picked for the binding, stored on the binding row.
//!
//! The two steps a crash must not split happen in one transaction each:
//! creating a session and recording its key (`binding_session`), and
//! accepting a message and recording its dedupe id (`binding_pending`,
//! `binding_dedupe`). A message is accepted once its row is written; the
//! session's next run takes every waiting row, and a session runs one at a
//! time, so a message sent during a run waits for it to end.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use diesel::OptionalExtension;
use diesel::sql_types::{BigInt, Integer, Text, Uuid as SqlUuid};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use plugin::daemon::{
    BindingHandle, DEDUPE_WINDOW, DaemonHost, PENDING_CAP, SendError, SessionError, SessionRef,
    SharedStore, check_send, valid_key,
};
use plugin::{Json, Message, SessionCtx};
use uuid::Uuid;

use super::storage::Stores;
use super::{BindingKey, Lease};
use crate::db::DbPool;

/// Characters of a session title a binding may set; longer ones are cut.
const TITLE_CHARS: usize = 200;

/// Starts the run that takes a session's waiting messages. The host's is
/// [`StateLauncher`]; tests use a fake.
#[async_trait]
pub trait Launcher: Send + Sync + 'static {
    /// Whether a run of `session` is under way in this process.
    fn running(&self, session: Uuid) -> bool;

    /// Starts a run of `session` with `text` as the user's turn, on the model
    /// `binding` names. Returns once the run is claimed, so `running` answers
    /// `true` from then until it ends.
    async fn launch(&self, session: Uuid, binding: &BindingKey, text: String)
    -> Result<(), String>;
}

/// The operator's ceilings on what one binding may start, whatever its
/// owner sets. They bound what channel members can spend of the owner's
/// credentials; the owner's own sessions are never counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ceilings {
    /// Live sessions one binding holds; `open` past it answers `limit`.
    pub sessions: u32,
    /// Runs of one binding's sessions at once; more messages wait.
    pub runs: u32,
    /// Messages one binding sends a minute; past it `send` answers `busy`.
    pub messages_per_minute: u32,
}

impl Default for Ceilings {
    fn default() -> Self {
        Ceilings {
            sessions: 100,
            runs: 2,
            messages_per_minute: 30,
        }
    }
}

impl Ceilings {
    /// `FABER_BINDING_MAX_SESSIONS`, `FABER_BINDING_MAX_RUNS` and
    /// `FABER_BINDING_MESSAGES_PER_MINUTE`, each a positive whole number;
    /// unset or blank is the default. A value that does not parse is refused.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Ceilings, String> {
        let read = |name: &str, default: u32| -> Result<u32, String> {
            match var(name).as_deref().map(str::trim) {
                None | Some("") => Ok(default),
                Some(text) => match text.parse::<u32>() {
                    Ok(value) if value > 0 => Ok(value),
                    _ => Err(format!(
                        "{name} must be a positive whole number, not `{text}`"
                    )),
                },
            }
        };
        let defaults = Ceilings::default();
        Ok(Ceilings {
            sessions: read("FABER_BINDING_MAX_SESSIONS", defaults.sessions)?,
            runs: read("FABER_BINDING_MAX_RUNS", defaults.runs)?,
            messages_per_minute: read(
                "FABER_BINDING_MESSAGES_PER_MINUTE",
                defaults.messages_per_minute,
            )?,
        })
    }

    pub fn from_env() -> Ceilings {
        Ceilings::from_vars(|name| std::env::var(name).ok())
            .unwrap_or_else(|message| panic!("{message}"))
    }

    /// The owner's values, each held to its ceiling.
    fn with(self, owner: Option<rows::OwnerLimits>) -> Ceilings {
        let Some(owner) = owner else {
            return self;
        };
        let held = |set: Option<i32>, ceiling: u32| {
            set.and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .map_or(ceiling, |value| value.min(ceiling))
        };
        Ceilings {
            sessions: held(owner.max_sessions, self.sessions),
            runs: held(owner.max_runs, self.runs),
            messages_per_minute: held(owner.messages_per_minute, self.messages_per_minute),
        }
    }
}

/// The window message rates are counted over.
const RATE_WINDOW: Duration = Duration::from_secs(60);

/// The binding session calls, for every binding of every plugin.
pub struct Sessions {
    db: DbPool,
    stores: Stores,
    pending_cap: usize,
    ceilings: Ceilings,
    launcher: OnceLock<Arc<dyn Launcher>>,
    /// One gate per session with a kick under way, so two kicks never both
    /// see "idle" and start two runs.
    gates: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
    /// When each binding's recent messages were accepted, in this process.
    sent: Mutex<HashMap<BindingKey, VecDeque<Instant>>>,
    /// The sessions of each binding with a run this host started for it.
    running: Mutex<HashMap<BindingKey, HashSet<Uuid>>>,
}

/// How a binding relates to a session.
enum Owner {
    /// No such session.
    Missing,
    /// It exists and this binding did not open it.
    Other,
    /// This binding opened it; `true` when it has been archived.
    Mine { archived: bool },
}

/// Rows of the raw queries below.
// The derive expands each field as `field: field`, which clippy flags.
#[allow(clippy::redundant_field_names)]
mod rows {
    use diesel::sql_types::{BigInt, Nullable, Text, Uuid as SqlUuid};
    use uuid::Uuid;

    #[derive(diesel::QueryableByName)]
    pub struct OwnerLimits {
        #[diesel(sql_type = Nullable<diesel::sql_types::Integer>)]
        pub max_sessions: Option<i32>,
        #[diesel(sql_type = Nullable<diesel::sql_types::Integer>)]
        pub max_runs: Option<i32>,
        #[diesel(sql_type = Nullable<diesel::sql_types::Integer>)]
        pub messages_per_minute: Option<i32>,
    }

    #[derive(diesel::QueryableByName)]
    pub struct Opened {
        #[diesel(sql_type = SqlUuid)]
        pub session_id: Uuid,
        #[diesel(sql_type = Nullable<BigInt>)]
        pub closed_at: Option<i64>,
    }

    #[derive(diesel::QueryableByName)]
    pub struct Ownership {
        #[diesel(sql_type = Nullable<SqlUuid>)]
        pub project_id: Option<Uuid>,
        #[diesel(sql_type = Nullable<Text>)]
        pub plugin_type: Option<String>,
        #[diesel(sql_type = Nullable<BigInt>)]
        pub closed_at: Option<i64>,
    }

    #[derive(diesel::QueryableByName)]
    pub struct Waiting {
        #[diesel(sql_type = BigInt)]
        pub id: i64,
        #[diesel(sql_type = Text)]
        pub content: String,
    }

    #[derive(diesel::QueryableByName)]
    pub struct Count {
        #[diesel(sql_type = BigInt)]
        pub count: i64,
    }

    #[derive(diesel::QueryableByName)]
    pub struct KeyRow {
        #[diesel(sql_type = Text)]
        pub key: String,
    }

    #[derive(diesel::QueryableByName)]
    pub struct SessionId {
        #[diesel(sql_type = SqlUuid)]
        pub session_id: Uuid,
    }
}

use rows::{Count, KeyRow, Opened, Ownership, SessionId, Waiting};

/// Why a transaction body stopped: an answer for the caller, or the database.
enum Failed<E> {
    Answer(E),
    Db(String),
}

impl<E> From<diesel::result::Error> for Failed<E> {
    fn from(error: diesel::result::Error) -> Self {
        Failed::Db(error.to_string())
    }
}

impl<E> From<crate::error::AppError> for Failed<E> {
    fn from(error: crate::error::AppError) -> Self {
        Failed::Db(error.to_string())
    }
}

fn logged(error: impl std::fmt::Display) {
    tracing::warn!(%error, "binding session call failed");
}

impl From<Failed<SessionError>> for SessionError {
    fn from(failed: Failed<SessionError>) -> Self {
        match failed {
            Failed::Answer(answer) => answer,
            Failed::Db(error) => {
                logged(error);
                SessionError::Unavailable
            }
        }
    }
}

impl From<Failed<SendError>> for SendError {
    fn from(failed: Failed<SendError>) -> Self {
        match failed {
            Failed::Answer(answer) => answer,
            Failed::Db(error) => {
                logged(error);
                SendError::Unavailable
            }
        }
    }
}

async fn lock(conn: &mut AsyncPgConnection, name: String) -> Result<(), diesel::result::Error> {
    diesel::sql_query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind::<Text, _>(name)
        .execute(conn)
        .await
        .map(|_| ())
}

async fn ownership(
    conn: &mut AsyncPgConnection,
    binding: &BindingKey,
    session: Uuid,
) -> Result<Owner, diesel::result::Error> {
    let row: Option<Ownership> = diesel::sql_query(
        "SELECT b.project_id, b.plugin_type, s.closed_at \
         FROM session s LEFT JOIN binding_session b ON b.session_id = s.id \
         WHERE s.id = $1",
    )
    .bind::<SqlUuid, _>(session)
    .get_result(conn)
    .await
    .optional()?;
    Ok(match row {
        None => Owner::Missing,
        Some(row)
            if row.project_id == Some(binding.project)
                && row.plugin_type.as_deref() == Some(binding.plugin.as_str()) =>
        {
            Owner::Mine {
                archived: row.closed_at.is_some(),
            }
        }
        Some(_) => Owner::Other,
    })
}

fn title_of(title: Option<&str>) -> Option<String> {
    let title = title?.trim();
    if title.is_empty() {
        return None;
    }
    Some(title.chars().take(TITLE_CHARS).collect())
}

impl Sessions {
    pub fn new(db: DbPool, stores: Stores) -> Self {
        Sessions {
            db,
            stores,
            pending_cap: PENDING_CAP,
            ceilings: Ceilings::default(),
            launcher: OnceLock::new(),
            gates: Mutex::new(HashMap::new()),
            sent: Mutex::new(HashMap::new()),
            running: Mutex::new(HashMap::new()),
        }
    }

    /// The operator's ceilings on every binding.
    pub fn with_ceilings(mut self, ceilings: Ceilings) -> Self {
        self.ceilings = ceilings;
        self
    }

    /// What `binding` may start: its owner's limits held to the ceilings.
    async fn limits(
        &self,
        conn: &mut AsyncPgConnection,
        binding: &BindingKey,
    ) -> Result<Ceilings, diesel::result::Error> {
        let owner: Option<rows::OwnerLimits> = diesel::sql_query(
            "SELECT max_sessions, max_runs, messages_per_minute FROM project_binding \
             WHERE project_id = $1 AND plugin_type = $2",
        )
        .bind::<SqlUuid, _>(binding.project)
        .bind::<Text, _>(&binding.plugin)
        .get_result(conn)
        .await
        .optional()?;
        Ok(self.ceilings.with(owner))
    }

    /// Takes one of `binding`'s messages for this minute, or refuses.
    fn take_message(&self, binding: &BindingKey, per_minute: u32) -> Option<Instant> {
        let now = Instant::now();
        let mut sent = self.sent.lock().expect("message rates poisoned");
        let recent = sent.entry(binding.clone()).or_default();
        while recent
            .front()
            .is_some_and(|at| now.duration_since(*at) >= RATE_WINDOW)
        {
            recent.pop_front();
        }
        if recent.len() >= per_minute as usize {
            return None;
        }
        recent.push_back(now);
        Some(now)
    }

    /// Gives back a message `take_message` took, for one that was not
    /// accepted after all.
    fn give_back(&self, binding: &BindingKey, taken: Instant) {
        let mut sent = self.sent.lock().expect("message rates poisoned");
        if let Some(recent) = sent.get_mut(binding)
            && let Some(at) = recent.iter().rposition(|at| *at == taken)
        {
            recent.remove(at);
        }
    }

    /// At most `cap` messages wait on one session.
    pub fn with_pending_cap(mut self, cap: usize) -> Self {
        self.pending_cap = cap;
        self
    }

    /// What starts runs. Set once, after the server state exists; until then
    /// accepted messages wait.
    pub fn attach(&self, launcher: Arc<dyn Launcher>) {
        let _ = self.launcher.set(launcher);
    }

    /// The handle a daemon gets for `binding`, live until `lease` is retired.
    pub fn handle(
        self: &Arc<Self>,
        binding: BindingKey,
        config: Json,
        lease: Lease,
    ) -> HostBinding {
        HostBinding {
            id: binding.id(),
            store: self.stores.for_handle(binding.clone(), lease.clone()),
            binding,
            config,
            lease,
            sessions: Arc::clone(self),
        }
    }

    /// Get-or-create by key. One transaction: a crash cannot leave a session
    /// without its key, and two opens of one key cannot make two sessions.
    pub async fn open(
        &self,
        binding: &BindingKey,
        key: &str,
        title: Option<&str>,
    ) -> Result<SessionRef, SessionError> {
        if !valid_key(key) {
            return Err(SessionError::InvalidKey);
        }
        let mut conn = self.db.get().await.map_err(|error| {
            logged(error);
            SessionError::Unavailable
        })?;
        let binding = binding.clone();
        let key = key.to_owned();
        let title = title_of(title);
        let limits = self.limits(&mut conn, &binding).await.map_err(|error| {
            logged(error);
            SessionError::Unavailable
        })?;
        let opened = conn
            .transaction::<SessionRef, Failed<SessionError>, _>(move |conn| {
                async move {
                    // Per binding, not per key: the session count below must
                    // not be passed by two opens of different keys at once.
                    lock(conn, format!("binding_session:{binding}")).await?;
                    let live: Option<Opened> = diesel::sql_query(
                        "SELECT b.session_id, s.closed_at \
                         FROM binding_session b JOIN session s ON s.id = b.session_id \
                         WHERE b.project_id = $1 AND b.plugin_type = $2 AND b.key = $3 \
                           AND b.released_at IS NULL",
                    )
                    .bind::<SqlUuid, _>(binding.project)
                    .bind::<Text, _>(&binding.plugin)
                    .bind::<Text, _>(&key)
                    .get_result(conn)
                    .await
                    .optional()?;
                    match live {
                        Some(live) if live.closed_at.is_none() => {
                            return Ok(SessionRef {
                                id: live.session_id.to_string(),
                                created: false,
                            });
                        }
                        // Archived: the key moves to a new session; the old
                        // one stays this binding's.
                        Some(live) => {
                            diesel::sql_query(
                                "UPDATE binding_session SET released_at = now() \
                                 WHERE session_id = $1",
                            )
                            .bind::<SqlUuid, _>(live.session_id)
                            .execute(conn)
                            .await?;
                        }
                        None => {}
                    }

                    let live: Count = diesel::sql_query(
                        "SELECT count(*) AS count \
                         FROM binding_session b JOIN session s ON s.id = b.session_id \
                         WHERE b.project_id = $1 AND b.plugin_type = $2 \
                           AND b.released_at IS NULL AND s.closed_at IS NULL",
                    )
                    .bind::<SqlUuid, _>(binding.project)
                    .bind::<Text, _>(&binding.plugin)
                    .get_result(conn)
                    .await?;
                    if live.count >= i64::from(limits.sessions) {
                        return Err(Failed::Answer(SessionError::Limit));
                    }

                    use crate::schema::{project, session, thread};
                    use diesel::{ExpressionMethods, QueryDsl};
                    let owner: Option<Uuid> = project::table
                        .filter(project::id.eq(binding.project))
                        .select(project::owner_id)
                        .first(conn)
                        .await
                        .optional()?;
                    let owner = owner.ok_or(Failed::Answer(SessionError::Gone))?;
                    let workspace = crate::access::personal_workspace(conn, owner).await?;
                    let id = Uuid::now_v7();
                    let now = crate::models::now_epoch();
                    diesel::insert_into(session::table)
                        .values(crate::models::session::NewSession {
                            id,
                            workspace_id: workspace.id,
                            title: title.as_deref(),
                            created_at: now,
                            project_id: Some(binding.project),
                        })
                        .execute(conn)
                        .await?;
                    diesel::insert_into(thread::table)
                        .values(crate::models::thread::NewThread {
                            id: Uuid::now_v7(),
                            session_id: id,
                            parent_id: None,
                            forked_at_seq: None,
                            created_at: now,
                        })
                        .execute(conn)
                        .await?;
                    diesel::sql_query(
                        "INSERT INTO binding_session (session_id, project_id, plugin_type, key) \
                         VALUES ($1, $2, $3, $4)",
                    )
                    .bind::<SqlUuid, _>(id)
                    .bind::<SqlUuid, _>(binding.project)
                    .bind::<Text, _>(&binding.plugin)
                    .bind::<Text, _>(&key)
                    .execute(conn)
                    .await?;
                    Ok(SessionRef {
                        id: id.to_string(),
                        created: true,
                    })
                }
                .scope_boxed()
            })
            .await?;
        Ok(opened)
    }

    /// Accepts `message` for `session` and returns once it is written. A
    /// `dedupe` id already recorded against the session makes this a
    /// successful no-op. An idle session starts its run now; a busy one takes
    /// the message when its run ends.
    pub async fn send(
        &self,
        binding: &BindingKey,
        session: &str,
        message: Message,
        dedupe: Option<&str>,
    ) -> Result<(), SendError> {
        check_send(&message, dedupe)?;
        let session: Uuid = session.parse().map_err(|_| SendError::NotFound)?;
        let mut conn = self.db.get().await.map_err(|error| {
            logged(error);
            SendError::Unavailable
        })?;
        let limits = self.limits(&mut conn, binding).await.map_err(|error| {
            logged(error);
            SendError::Unavailable
        })?;
        let taken = self
            .take_message(binding, limits.messages_per_minute)
            .ok_or(SendError::Busy)?;
        let rate = binding.clone();
        let binding = binding.clone();
        let dedupe = dedupe.map(str::to_owned);
        let content = message.text();
        let cap = self.pending_cap as i64;
        let window = DEDUPE_WINDOW.as_secs() as i32;
        let accepted = conn
            .transaction::<bool, Failed<SendError>, _>(move |conn| {
                async move {
                    lock(conn, format!("binding_pending:{session}")).await?;
                    match ownership(conn, &binding, session).await? {
                        Owner::Missing => return Err(Failed::Answer(SendError::NotFound)),
                        Owner::Other => return Err(Failed::Answer(SendError::NotOwned)),
                        Owner::Mine { archived: true } => {
                            return Err(Failed::Answer(SendError::Archived));
                        }
                        Owner::Mine { archived: false } => {}
                    }
                    if let Some(dedupe) = &dedupe {
                        diesel::sql_query(
                            "DELETE FROM binding_dedupe WHERE session_id = $1 \
                             AND created_at < now() - make_interval(secs => $2)",
                        )
                        .bind::<SqlUuid, _>(session)
                        .bind::<Integer, _>(window)
                        .execute(conn)
                        .await?;
                        let recorded = diesel::sql_query(
                            "INSERT INTO binding_dedupe (session_id, dedupe_id) VALUES ($1, $2) \
                             ON CONFLICT DO NOTHING",
                        )
                        .bind::<SqlUuid, _>(session)
                        .bind::<Text, _>(dedupe)
                        .execute(conn)
                        .await?;
                        if recorded == 0 {
                            return Ok(false);
                        }
                    }
                    let waiting: Count = diesel::sql_query(
                        "SELECT count(*) AS count FROM binding_pending WHERE session_id = $1",
                    )
                    .bind::<SqlUuid, _>(session)
                    .get_result(conn)
                    .await?;
                    if waiting.count >= cap {
                        // Rolls the dedupe id back too: a refused message was
                        // never seen.
                        return Err(Failed::Answer(SendError::Busy));
                    }
                    diesel::sql_query(
                        "INSERT INTO binding_pending (session_id, content) VALUES ($1, $2)",
                    )
                    .bind::<SqlUuid, _>(session)
                    .bind::<Text, _>(&content)
                    .execute(conn)
                    .await?;
                    Ok(true)
                }
                .scope_boxed()
            })
            .await;
        drop(conn);
        // A message that was refused, or was a replay, did not count.
        if !matches!(accepted, Ok(true)) {
            self.give_back(&rate, taken);
        }
        if accepted? {
            self.kick(session).await;
        }
        Ok(())
    }

    /// Soft and idempotent: the session is closed, as the owner closes one,
    /// and messages still waiting on it are dropped.
    pub async fn archive(&self, binding: &BindingKey, session: &str) -> Result<(), SessionError> {
        let session: Uuid = session.parse().map_err(|_| SessionError::NotFound)?;
        let mut conn = self.db.get().await.map_err(|error| {
            logged(error);
            SessionError::Unavailable
        })?;
        let binding = binding.clone();
        conn.transaction::<(), Failed<SessionError>, _>(move |conn| {
            async move {
                lock(conn, format!("binding_pending:{session}")).await?;
                match ownership(conn, &binding, session).await? {
                    Owner::Missing => return Err(Failed::Answer(SessionError::NotFound)),
                    Owner::Other => return Err(Failed::Answer(SessionError::NotOwned)),
                    Owner::Mine { .. } => {}
                }
                diesel::sql_query(
                    "UPDATE session SET closed_at = $2 WHERE id = $1 AND closed_at IS NULL",
                )
                .bind::<SqlUuid, _>(session)
                .bind::<BigInt, _>(crate::models::now_epoch())
                .execute(conn)
                .await?;
                diesel::sql_query("DELETE FROM binding_pending WHERE session_id = $1")
                    .bind::<SqlUuid, _>(session)
                    .execute(conn)
                    .await?;
                Ok(())
            }
            .scope_boxed()
        })
        .await?;
        Ok(())
    }

    /// The key `plugin`'s binding opened `session` under, if it opened it.
    pub async fn key_of(&self, plugin: &str, session: &str) -> Option<String> {
        let session: Uuid = session.parse().ok()?;
        let mut conn = self.db.get().await.map_err(logged).ok()?;
        let row: Option<KeyRow> = diesel::sql_query(
            "SELECT key FROM binding_session WHERE session_id = $1 AND plugin_type = $2",
        )
        .bind::<SqlUuid, _>(session)
        .bind::<Text, _>(plugin)
        .get_result(&mut conn)
        .await
        .optional()
        .map_err(logged)
        .ok()?;
        row.map(|row| row.key)
    }

    /// A run of some session ended: if it is one a binding opened and
    /// messages are waiting on it, its next run takes them.
    pub async fn run_ended(&self, session: Uuid) {
        let freed = {
            let mut running = self.running.lock().expect("binding runs poisoned");
            running
                .iter_mut()
                .find_map(|(binding, sessions)| sessions.remove(&session).then(|| binding.clone()))
        };
        self.kick(session).await;
        // A run slot of that binding came free: its other waiting sessions
        // may take it.
        if let Some(binding) = freed {
            for other in self.waiting_of(&binding).await {
                if other != session {
                    self.kick(other).await;
                }
            }
        }
    }

    /// `binding`'s sessions with messages waiting, oldest message first.
    async fn waiting_of(&self, binding: &BindingKey) -> Vec<Uuid> {
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return Vec::new();
        };
        let rows: Vec<SessionId> = match diesel::sql_query(
            "SELECT p.session_id FROM binding_pending p \
             JOIN binding_session b ON b.session_id = p.session_id \
             WHERE b.project_id = $1 AND b.plugin_type = $2 \
             GROUP BY p.session_id ORDER BY min(p.id)",
        )
        .bind::<SqlUuid, _>(binding.project)
        .bind::<Text, _>(&binding.plugin)
        .load(&mut conn)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                logged(error);
                return Vec::new();
            }
        };
        rows.into_iter().map(|row| row.session_id).collect()
    }

    /// Takes a run slot of `binding` for `session`, or `false` when the
    /// binding already runs as many as it may. Slots of runs that ended
    /// without telling us are reclaimed first.
    fn take_run(
        &self,
        launcher: &dyn Launcher,
        binding: &BindingKey,
        session: Uuid,
        cap: u32,
    ) -> bool {
        let mut running = self.running.lock().expect("binding runs poisoned");
        let sessions = running.entry(binding.clone()).or_default();
        sessions.retain(|other| launcher.running(*other));
        if sessions.len() >= cap as usize {
            return false;
        }
        sessions.insert(session);
        true
    }

    fn give_back_run(&self, binding: &BindingKey, session: Uuid) {
        if let Some(sessions) = self
            .running
            .lock()
            .expect("binding runs poisoned")
            .get_mut(binding)
        {
            sessions.remove(&session);
        }
    }

    /// Kicks every session with waiting messages: what a process that died
    /// before running them left behind. Called once at startup.
    pub async fn recover(&self) {
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return;
        };
        let waiting: Vec<SessionId> =
            match diesel::sql_query("SELECT DISTINCT session_id FROM binding_pending")
                .load(&mut conn)
                .await
            {
                Ok(rows) => rows,
                Err(error) => return logged(error),
            };
        drop(conn);
        for row in waiting {
            self.kick(row.session_id).await;
        }
    }

    fn gate(&self, session: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.gates
                .lock()
                .expect("session gates poisoned")
                .entry(session)
                .or_default(),
        )
    }

    /// Starts a run of `session` with everything waiting on it, unless one is
    /// under way (its end kicks again) or nothing waits. Serialized per
    /// session: the "idle?" check and the claim of the new run happen under
    /// one gate, so two kicks cannot both start a run.
    async fn kick(&self, session: Uuid) {
        let Some(launcher) = self.launcher.get() else {
            return;
        };
        let gate = self.gate(session);
        {
            let _held = gate.lock().await;
            if !launcher.running(session) {
                self.start(launcher.as_ref(), session).await;
            }
        }
        // Forget the gate once nobody else holds it.
        let mut gates = self.gates.lock().expect("session gates poisoned");
        if Arc::strong_count(&gate) == 2 {
            gates.remove(&session);
        }
    }

    async fn start(&self, launcher: &dyn Launcher, session: Uuid) {
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return;
        };
        let owner: Option<Ownership> = match diesel::sql_query(
            "SELECT b.project_id, b.plugin_type, s.closed_at \
             FROM binding_session b JOIN session s ON s.id = b.session_id \
             WHERE b.session_id = $1",
        )
        .bind::<SqlUuid, _>(session)
        .get_result(&mut conn)
        .await
        .optional()
        {
            Ok(owner) => owner,
            Err(error) => return logged(error),
        };
        // Not a binding's session, or archived: nothing of ours waits on it.
        let Some(Ownership {
            project_id: Some(project),
            plugin_type: Some(plugin),
            closed_at: None,
        }) = owner
        else {
            return;
        };
        let waiting: Vec<Waiting> = match diesel::sql_query(
            "SELECT id, content FROM binding_pending WHERE session_id = $1 ORDER BY id",
        )
        .bind::<SqlUuid, _>(session)
        .load(&mut conn)
        .await
        {
            Ok(rows) => rows,
            Err(error) => return logged(error),
        };
        let Some(last) = waiting.last().map(|row| row.id) else {
            return;
        };
        let binding = BindingKey::new(project, plugin);
        let limits = match self.limits(&mut conn, &binding).await {
            Ok(limits) => limits,
            Err(error) => return logged(error),
        };
        drop(conn);
        // The binding's other sessions hold every run it may have: this one
        // waits for one of them to end.
        if !self.take_run(launcher, &binding, session, limits.runs) {
            return;
        }

        // One user turn: providers want turns to alternate, and the messages
        // were all said before the agent could answer any of them.
        let text = waiting
            .iter()
            .map(|row| row.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        if let Err(error) = launcher.launch(session, &binding, text).await {
            self.give_back_run(&binding, session);
            // Left waiting: the owner fixing the binding's model, or the next
            // message, kicks again.
            tracing::warn!(%session, %binding, %error, "messages a binding sent wait: no run started");
            return;
        }
        // The run has recorded them as its input. A crash before this line
        // delivers them twice; one after it, never — the run already has them.
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return;
        };
        if let Err(error) =
            diesel::sql_query("DELETE FROM binding_pending WHERE session_id = $1 AND id <= $2")
                .bind::<SqlUuid, _>(session)
                .bind::<BigInt, _>(last)
                .execute(&mut conn)
                .await
        {
            logged(error);
        }
    }
}

/// `resource binding`: one binding's handle, as the host gives it to a
/// daemon.
pub struct HostBinding {
    id: String,
    binding: BindingKey,
    config: Json,
    lease: Lease,
    store: SharedStore,
    sessions: Arc<Sessions>,
}

impl HostBinding {
    pub fn key(&self) -> &BindingKey {
        &self.binding
    }

    pub fn lease(&self) -> &Lease {
        &self.lease
    }
}

#[async_trait]
impl BindingHandle for HostBinding {
    fn id(&self) -> &str {
        &self.id
    }

    fn config(&self) -> &Json {
        &self.config
    }

    async fn open(&self, key: &str, title: Option<&str>) -> Result<SessionRef, SessionError> {
        if self.lease.is_retired() {
            return Err(SessionError::Gone);
        }
        self.sessions.open(&self.binding, key, title).await
    }

    async fn send(
        &self,
        session: &str,
        message: Message,
        dedupe: Option<&str>,
    ) -> Result<(), SendError> {
        if self.lease.is_retired() {
            return Err(SendError::Gone);
        }
        self.sessions
            .send(&self.binding, session, message, dedupe)
            .await
    }

    async fn archive(&self, session: &str) -> Result<(), SessionError> {
        if self.lease.is_retired() {
            return Err(SessionError::Gone);
        }
        self.sessions.archive(&self.binding, session).await
    }

    fn storage(&self) -> SharedStore {
        Arc::clone(&self.store)
    }
}

/// The rest of `interface host`, for tool handlers: the binding is the one
/// the calling session's context belongs to.
#[async_trait]
impl DaemonHost for Sessions {
    async fn key_of(&self, ctx: &SessionCtx) -> Option<String> {
        Sessions::key_of(self, ctx.binding(), ctx.session_id()).await
    }

    async fn store_of(&self, ctx: &SessionCtx) -> SharedStore {
        let project = match ctx.session_id().parse::<Uuid>() {
            Ok(session) => self.project_of(session).await,
            Err(_) => None,
        };
        // A session outside any project has no binding; its store refuses
        // every write (no such project) and holds nothing.
        self.stores.for_binding(BindingKey::new(
            project.unwrap_or_else(Uuid::nil),
            ctx.binding(),
        ))
    }
}

impl Sessions {
    async fn project_of(&self, session: Uuid) -> Option<Uuid> {
        use crate::schema::session as table;
        use diesel::{ExpressionMethods, QueryDsl};
        let mut conn = self.db.get().await.map_err(logged).ok()?;
        table::table
            .filter(table::id.eq(session))
            .select(table::project_id)
            .first::<Option<Uuid>>(&mut conn)
            .await
            .optional()
            .map_err(logged)
            .ok()
            .flatten()
            .flatten()
    }
}

/// The host's [`Launcher`]: a run as the owner would start one, on the model
/// the owner picked for the binding.
pub struct StateLauncher(pub crate::state::AppState);

#[async_trait]
impl Launcher for StateLauncher {
    fn running(&self, session: Uuid) -> bool {
        crate::run::is_running(&self.0.runs, session)
    }

    async fn launch(
        &self,
        session: Uuid,
        binding: &BindingKey,
        text: String,
    ) -> Result<(), String> {
        use crate::schema::{project, project_binding};
        use diesel::{ExpressionMethods, QueryDsl};
        let state = &self.0;
        let mut conn = state.db.get().await.map_err(|error| error.to_string())?;
        let (owner, model, thinking): (Uuid, Option<String>, Option<String>) =
            project_binding::table
                .inner_join(project::table)
                .filter(project_binding::project_id.eq(binding.project))
                .filter(project_binding::plugin_type.eq(&binding.plugin))
                .select((
                    project::owner_id,
                    project_binding::model_alias,
                    project_binding::thinking_effort,
                ))
                .first(&mut conn)
                .await
                .map_err(|error| format!("the binding is not there: {error}"))?;
        let model = model.ok_or("the owner has not picked a model for this binding")?;
        let resolved = crate::resolve::resolve_model(state, owner, &model)
            .await
            .map_err(|error| format!("model `{model}`: {error}"))?;
        let thinking = crate::models::thinking::ThinkingSelection::from_stored(thinking.as_deref());
        let thread = crate::routes::sessions::resolve_thread(&mut conn, session, None)
            .await
            .map_err(|error| error.to_string())?;
        let input = vec![crate::run::TurnMessage {
            kind: crate::run::KIND_INPUT,
            message: llm::Message {
                role: llm::Role::User,
                content: vec![llm::ContentBlock::Text { text }],
            },
        }];
        crate::routes::sessions::launch_run(
            state,
            conn,
            crate::routes::sessions::Launch {
                session_id: session,
                thread_id: thread,
                user_id: owner,
                resolved,
                thinking,
                input,
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::plugins::daemon::fixture::Fixture;
    use crate::plugins::daemon::storage::Limits;
    use diesel::sql_types::Nullable;

    /// Records launches; a session is "running" until the test ends it.
    #[derive(Default)]
    struct FakeLauncher {
        running: Mutex<HashSet<Uuid>>,
        launched: Mutex<Vec<(Uuid, String)>>,
        refuse: std::sync::atomic::AtomicBool,
    }

    impl FakeLauncher {
        fn launched(&self) -> Vec<(Uuid, String)> {
            self.launched.lock().unwrap().clone()
        }
        fn end(&self, session: Uuid) {
            self.running.lock().unwrap().remove(&session);
        }
    }

    #[async_trait]
    impl Launcher for FakeLauncher {
        fn running(&self, session: Uuid) -> bool {
            self.running.lock().unwrap().contains(&session)
        }
        async fn launch(&self, session: Uuid, _: &BindingKey, text: String) -> Result<(), String> {
            if std::sync::atomic::AtomicBool::load(
                &self.refuse,
                std::sync::atomic::Ordering::SeqCst,
            ) {
                return Err("no model".into());
            }
            self.running.lock().unwrap().insert(session);
            self.launched.lock().unwrap().push((session, text));
            Ok(())
        }
    }

    fn sessions(fixture: &Fixture) -> (Arc<Sessions>, Arc<FakeLauncher>) {
        let sessions = Arc::new(
            Sessions::new(
                fixture.pool.clone(),
                Stores::new(fixture.pool.clone(), Limits::default()),
            )
            .with_pending_cap(2),
        );
        let launcher = Arc::new(FakeLauncher::default());
        sessions.attach(launcher.clone());
        (sessions, launcher)
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn open_is_get_or_create_and_keys_are_scoped_to_the_binding() {
        let fixture = Fixture::new(0).await;
        let (sessions, _) = sessions(&fixture);
        let discord = fixture.binding("discord");
        let first = sessions
            .open(&discord, "thread:1", Some("hi"))
            .await
            .unwrap();
        assert!(first.created);
        let again = sessions.open(&discord, "thread:1", None).await.unwrap();
        assert_eq!(
            (again.id.as_str(), again.created),
            (first.id.as_str(), false)
        );

        // The same key in another project, or for another plugin, is another
        // session.
        let elsewhere = BindingKey::new(fixture.project("other").await, "discord");
        let theirs = sessions.open(&elsewhere, "thread:1", None).await.unwrap();
        let slack = sessions
            .open(&fixture.binding("slack"), "thread:1", None)
            .await
            .unwrap();
        assert!(theirs.created && slack.created);
        assert_ne!(theirs.id, first.id);
        assert_ne!(slack.id, first.id);

        assert_eq!(
            sessions.open(&discord, "", None).await,
            Err(SessionError::InvalidKey)
        );
        assert_eq!(
            sessions.key_of("discord", &first.id).await.as_deref(),
            Some("thread:1")
        );
        assert_eq!(sessions.key_of("slack", &first.id).await, None);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn concurrent_opens_of_one_key_make_one_session() {
        let fixture = Fixture::new(0).await;
        let (sessions, _) = sessions(&fixture);
        let discord = fixture.binding("discord");
        let opens = (0..6).map(|_| {
            let sessions = Arc::clone(&sessions);
            let discord = discord.clone();
            async move { sessions.open(&discord, "thread:9", None).await.unwrap() }
        });
        let refs = futures::future::join_all(opens).await;
        let ids: HashSet<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(refs.iter().filter(|r| r.created).count(), 1);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_replayed_dedupe_id_does_not_post_twice() {
        let fixture = Fixture::new(0).await;
        let (sessions, launcher) = sessions(&fixture);
        let discord = fixture.binding("discord");
        let session = sessions.open(&discord, "thread:1", None).await.unwrap().id;
        sessions
            .send(&discord, &session, Message::user("hello"), Some("m1"))
            .await
            .unwrap();
        launcher.end(session.parse().unwrap());
        sessions
            .send(&discord, &session, Message::user("hello"), Some("m1"))
            .await
            .unwrap();
        assert_eq!(launcher.launched().len(), 1);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_message_sent_during_a_run_is_delivered_after_it() {
        let fixture = Fixture::new(0).await;
        let (sessions, launcher) = sessions(&fixture);
        let discord = fixture.binding("discord");
        let session = sessions.open(&discord, "thread:1", None).await.unwrap().id;
        let id: Uuid = session.parse().unwrap();

        sessions
            .send(&discord, &session, Message::user("first"), None)
            .await
            .unwrap();
        // The run is under way: these wait.
        for text in ["second", "third"] {
            sessions
                .send(&discord, &session, Message::user(text), None)
                .await
                .unwrap();
        }
        assert_eq!(launcher.launched(), [(id, "first".to_owned())]);

        launcher.end(id);
        sessions.run_ended(id).await;
        assert_eq!(
            launcher.launched(),
            [(id, "first".to_owned()), (id, "second\n\nthird".to_owned())]
        );
        // Taken: the next run end finds nothing.
        launcher.end(id);
        sessions.run_ended(id).await;
        assert_eq!(launcher.launched().len(), 2);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn the_pending_cap_answers_busy_for_that_session_only() {
        let fixture = Fixture::new(0).await;
        let (sessions, launcher) = sessions(&fixture);
        let discord = fixture.binding("discord");
        let busy = sessions.open(&discord, "thread:1", None).await.unwrap().id;
        let calm = sessions.open(&discord, "thread:2", None).await.unwrap().id;

        sessions
            .send(&discord, &busy, Message::user("starts a run"), None)
            .await
            .unwrap();
        for n in 0..2 {
            sessions
                .send(&discord, &busy, Message::user(format!("waits {n}")), None)
                .await
                .unwrap();
        }
        assert_eq!(
            sessions
                .send(&discord, &busy, Message::user("one too many"), Some("m9"))
                .await,
            Err(SendError::Busy)
        );
        // Another session of the same binding is not held up.
        sessions
            .send(&discord, &calm, Message::user("hi"), None)
            .await
            .unwrap();
        assert_eq!(launcher.launched().len(), 2);

        // The refused message's dedupe id was not kept: once there is room,
        // the same id goes through.
        launcher.end(busy.parse().unwrap());
        sessions.run_ended(busy.parse().unwrap()).await;
        sessions
            .send(&discord, &busy, Message::user("one too many"), Some("m9"))
            .await
            .unwrap();
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_binding_reaches_only_the_sessions_it_opened() {
        let fixture = Fixture::new(1).await;
        let (sessions, _) = sessions(&fixture);
        let discord = fixture.binding("discord");
        let slack = fixture.binding("slack");
        let owners = fixture.sessions[0].to_string();
        let slacks = sessions.open(&slack, "c:1", None).await.unwrap().id;

        for session in [&owners, &slacks] {
            assert_eq!(
                sessions
                    .send(&discord, session, Message::user("x"), None)
                    .await,
                Err(SendError::NotOwned)
            );
            assert_eq!(
                sessions.archive(&discord, session).await,
                Err(SessionError::NotOwned)
            );
        }
        let nobody = Uuid::now_v7().to_string();
        assert_eq!(
            sessions
                .send(&discord, &nobody, Message::user("x"), None)
                .await,
            Err(SendError::NotFound)
        );
        assert_eq!(
            sessions
                .send(&discord, "not-an-id", Message::user("x"), None)
                .await,
            Err(SendError::NotFound)
        );
        assert_eq!(
            sessions
                .send(&discord, &slacks, Message::system("x"), None)
                .await,
            Err(SendError::RoleRefused)
        );
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn archive_then_open_moves_the_key() {
        let fixture = Fixture::new(0).await;
        let (sessions, launcher) = sessions(&fixture);
        let discord = fixture.binding("discord");
        let old = sessions.open(&discord, "thread:1", None).await.unwrap().id;
        sessions.archive(&discord, &old).await.unwrap();
        sessions.archive(&discord, &old).await.unwrap();
        assert_eq!(
            sessions
                .send(&discord, &old, Message::user("late"), None)
                .await,
            Err(SendError::Archived)
        );

        let new = sessions.open(&discord, "thread:1", None).await.unwrap();
        assert!(new.created);
        assert_ne!(new.id, old);
        // Both stay the binding's; the key names the new one.
        assert_eq!(
            sessions.key_of("discord", &old).await.as_deref(),
            Some("thread:1")
        );
        sessions.archive(&discord, &old).await.unwrap();
        sessions
            .send(&discord, &new.id, Message::user("hi"), None)
            .await
            .unwrap();
        assert_eq!(launcher.launched().len(), 1);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn messages_wait_while_no_run_can_start_and_a_retired_handle_is_gone() {
        let fixture = Fixture::new(0).await;
        let (sessions, launcher) = sessions(&fixture);
        launcher
            .refuse
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let lease = Lease::new();
        let handle = sessions.handle(
            fixture.binding("discord"),
            serde_json::json!({}),
            lease.clone(),
        );
        let session = handle.open("thread:1", None).await.unwrap().id;
        handle
            .send(&session, Message::user("one"), None)
            .await
            .unwrap();
        assert!(launcher.launched().is_empty());

        // The owner fixes the binding; the next kick takes what waited.
        // (`recover` would kick every session waiting in the shared test
        // database, so this kicks its own.)
        launcher
            .refuse
            .store(false, std::sync::atomic::Ordering::SeqCst);
        sessions.run_ended(session.parse().unwrap()).await;
        assert_eq!(launcher.launched().len(), 1);
        assert_eq!(launcher.launched()[0].1, "one");

        lease.retire();
        assert_eq!(handle.open("thread:1", None).await, Err(SessionError::Gone));
        assert_eq!(
            handle.send(&session, Message::user("x"), None).await,
            Err(SendError::Gone)
        );
        assert_eq!(handle.archive(&session).await, Err(SessionError::Gone));
        fixture.cleanup().await;
    }

    fn limited(fixture: &Fixture, ceilings: Ceilings) -> (Arc<Sessions>, Arc<FakeLauncher>) {
        let sessions = Arc::new(
            Sessions::new(
                fixture.pool.clone(),
                Stores::new(fixture.pool.clone(), Limits::default()),
            )
            .with_ceilings(ceilings),
        );
        let launcher = Arc::new(FakeLauncher::default());
        sessions.attach(launcher.clone());
        (sessions, launcher)
    }

    async fn bind_with(fixture: &Fixture, plugin: &str, max_sessions: Option<i32>) {
        let mut conn = fixture.pool.get().await.unwrap();
        diesel::sql_query(
            "INSERT INTO project_binding \
             (project_id, plugin_type, version, config_version, config, max_sessions) \
             VALUES ($1, $2, '1.0.0', 1, '{}', $3)",
        )
        .bind::<SqlUuid, _>(fixture.project)
        .bind::<Text, _>(plugin)
        .bind::<Nullable<Integer>, _>(max_sessions)
        .execute(&mut conn)
        .await
        .unwrap();
    }

    #[test]
    fn ceilings_come_from_their_variables_and_hold_the_owner() {
        let ceilings = Ceilings::from_vars(|name| match name {
            "FABER_BINDING_MAX_RUNS" => Some(" 3 ".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(ceilings.runs, 3);
        assert_eq!(ceilings.sessions, Ceilings::default().sessions);
        assert!(Ceilings::from_vars(|_| Some("0".into())).is_err());
        let held = ceilings.with(Some(rows::OwnerLimits {
            max_sessions: Some(5),
            max_runs: Some(10),
            messages_per_minute: None,
        }));
        assert_eq!((held.sessions, held.runs), (5, 3));
        assert_eq!(held.messages_per_minute, ceilings.messages_per_minute);
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn the_session_limit_refuses_new_keys_only_for_that_binding() {
        let fixture = Fixture::new(3).await;
        let (sessions, _) = limited(&fixture, Ceilings::default());
        // The owner allows two; the owner's own three sessions do not count.
        bind_with(&fixture, "discord", Some(2)).await;
        let discord = fixture.binding("discord");
        let one = sessions.open(&discord, "t:1", None).await.unwrap();
        sessions.open(&discord, "t:2", None).await.unwrap();
        assert_eq!(
            sessions.open(&discord, "t:3", None).await,
            Err(SessionError::Limit)
        );
        // An existing key is still found, and another binding is unaffected.
        assert!(!sessions.open(&discord, "t:1", None).await.unwrap().created);
        sessions
            .open(&fixture.binding("slack"), "t:3", None)
            .await
            .unwrap();
        // Archiving one frees its place.
        sessions.archive(&discord, &one.id).await.unwrap();
        sessions.open(&discord, "t:3", None).await.unwrap();
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn runs_past_the_limit_wait_for_one_of_the_binding_to_end() {
        let fixture = Fixture::new(0).await;
        let (sessions, launcher) = limited(
            &fixture,
            Ceilings {
                runs: 1,
                ..Ceilings::default()
            },
        );
        let discord = fixture.binding("discord");
        let first = sessions.open(&discord, "t:1", None).await.unwrap().id;
        let second = sessions.open(&discord, "t:2", None).await.unwrap().id;
        let slacks = sessions
            .open(&fixture.binding("slack"), "t:1", None)
            .await
            .unwrap()
            .id;
        for session in [&first, &second] {
            sessions
                .send(&discord, session, Message::user("go"), None)
                .await
                .unwrap();
        }
        sessions
            .send(
                &fixture.binding("slack"),
                &slacks,
                Message::user("go"),
                None,
            )
            .await
            .unwrap();
        let ran: Vec<Uuid> = launcher.launched().iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ran,
            [first.parse::<Uuid>().unwrap(), slacks.parse().unwrap()]
        );

        launcher.end(first.parse().unwrap());
        sessions.run_ended(first.parse().unwrap()).await;
        assert_eq!(launcher.launched()[2].0, second.parse::<Uuid>().unwrap());
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn the_message_rate_answers_busy_and_a_replay_does_not_count() {
        let fixture = Fixture::new(0).await;
        let (sessions, _) = limited(
            &fixture,
            Ceilings {
                messages_per_minute: 2,
                ..Ceilings::default()
            },
        );
        let discord = fixture.binding("discord");
        let session = sessions.open(&discord, "t:1", None).await.unwrap().id;
        sessions
            .send(&discord, &session, Message::user("1"), Some("a"))
            .await
            .unwrap();
        sessions
            .send(&discord, &session, Message::user("1"), Some("a"))
            .await
            .unwrap();
        sessions
            .send(&discord, &session, Message::user("2"), None)
            .await
            .unwrap();
        assert_eq!(
            sessions
                .send(&discord, &session, Message::user("3"), None)
                .await,
            Err(SendError::Busy)
        );
        let slack = fixture.binding("slack");
        let theirs = sessions.open(&slack, "t:1", None).await.unwrap().id;
        sessions
            .send(&slack, &theirs, Message::user("fine"), None)
            .await
            .unwrap();
        fixture.cleanup().await;
    }
}
