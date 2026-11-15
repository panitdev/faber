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
//! accepting a message and recording its dedupe id. What a binding sends
//! waits in the session's inbox ([`crate::inbox`]) like a message from any
//! other sender; what is the binding's own stays here: which sessions it may
//! reach, how fast it may send, and how many of its sessions run at once.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use diesel::OptionalExtension;
use diesel::sql_types::{Text, Uuid as SqlUuid};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use plugin::daemon::{
    BindingHandle, DaemonHost, Envelope, SendError, SessionError, SessionRef, SharedStore,
    check_send, valid_key,
};
use plugin::{Json, SessionCtx};
use uuid::Uuid;

use super::storage::Stores;
use super::{BindingKey, Lease};
use crate::db::DbPool;
use crate::inbox::{self, Admission, Dedupe, Inbox, Launcher, Refused, Said};
use crate::said::Origin;

/// Characters of a session title a binding may set; longer ones are cut.
const TITLE_CHARS: usize = 200;

/// The operator's ceilings on what one binding may start, whatever its
/// owner sets. They bound what channel members can spend of the owner's
/// credentials; the owner's own sessions are never counted. `None` is no
/// ceiling, which is the default: only what an owner sets limits a binding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ceilings {
    /// Live sessions one binding holds; `open` past it answers `limit`.
    pub sessions: Option<u32>,
    /// Runs of one binding's sessions at once; more messages wait.
    pub runs: Option<u32>,
    /// Messages one binding sends a minute; past it `send` answers `busy`.
    pub messages_per_minute: Option<u32>,
}

impl Ceilings {
    /// `FABER_BINDING_MAX_SESSIONS`, `FABER_BINDING_MAX_RUNS` and
    /// `FABER_BINDING_MESSAGES_PER_MINUTE`, each a positive whole number;
    /// unset or blank is no ceiling. A value that does not parse is refused.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Ceilings, String> {
        let read = |name: &str| -> Result<Option<u32>, String> {
            match var(name).as_deref().map(str::trim) {
                None | Some("") => Ok(None),
                Some(text) => match text.parse::<u32>() {
                    Ok(value) if value > 0 => Ok(Some(value)),
                    _ => Err(format!(
                        "{name} must be a positive whole number, not `{text}`"
                    )),
                },
            }
        };
        Ok(Ceilings {
            sessions: read("FABER_BINDING_MAX_SESSIONS")?,
            runs: read("FABER_BINDING_MAX_RUNS")?,
            messages_per_minute: read("FABER_BINDING_MESSAGES_PER_MINUTE")?,
        })
    }

    pub fn from_env() -> Ceilings {
        Ceilings::from_vars(|name| std::env::var(name).ok())
            .unwrap_or_else(|message| panic!("{message}"))
    }

    /// The owner's values, each held to its ceiling; neither set is no limit.
    fn with(self, owner: Option<rows::OwnerLimits>) -> Ceilings {
        let Some(owner) = owner else {
            return self;
        };
        let held = |set: Option<i32>, ceiling: Option<u32>| {
            let set = set
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0);
            match (set, ceiling) {
                (Some(set), Some(ceiling)) => Some(set.min(ceiling)),
                (set, ceiling) => set.or(ceiling),
            }
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
    inbox: Arc<Inbox>,
    slots: Arc<Slots>,
    /// When each binding's recent messages were accepted, in this process.
    sent: Mutex<HashMap<BindingKey, VecDeque<Instant>>>,
}

/// What each binding may start, and the runs it has under way: what holds a
/// binding's session back when the binding already runs as many as it may.
struct Slots {
    db: DbPool,
    ceilings: Ceilings,
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

    /// The binding that opened a session.
    #[derive(diesel::QueryableByName)]
    pub struct Bound {
        #[diesel(sql_type = SqlUuid)]
        pub project_id: Uuid,
        #[diesel(sql_type = Text)]
        pub plugin_type: String,
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

use rows::{Bound, Count, KeyRow, Opened, Ownership, SessionId};

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

impl Slots {
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

    /// The binding that opened `session`, if one did.
    async fn binding_of(
        &self,
        conn: &mut AsyncPgConnection,
        session: Uuid,
    ) -> Result<Option<BindingKey>, diesel::result::Error> {
        let bound: Option<Bound> = diesel::sql_query(
            "SELECT project_id, plugin_type FROM binding_session WHERE session_id = $1",
        )
        .bind::<SqlUuid, _>(session)
        .get_result(conn)
        .await
        .optional()?;
        Ok(bound.map(|bound| BindingKey::new(bound.project_id, bound.plugin_type)))
    }

    /// `binding`'s sessions with messages waiting, oldest message first.
    async fn waiting_of(&self, binding: &BindingKey) -> Vec<Uuid> {
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return Vec::new();
        };
        let rows: Vec<SessionId> = match diesel::sql_query(
            "SELECT p.session_id FROM session_inbox p \
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
    fn take(
        &self,
        launcher: &dyn Launcher,
        binding: &BindingKey,
        session: Uuid,
        cap: Option<u32>,
    ) -> bool {
        let mut running = self.running.lock().expect("binding runs poisoned");
        let sessions = running.entry(binding.clone()).or_default();
        sessions.retain(|other| launcher.running(*other));
        if cap.is_some_and(|cap| sessions.len() >= cap as usize) {
            return false;
        }
        sessions.insert(session);
        true
    }

    /// Frees the slot `session` holds, and names the binding it was of.
    fn free(&self, session: Uuid) -> Option<BindingKey> {
        let mut running = self.running.lock().expect("binding runs poisoned");
        running
            .iter_mut()
            .find_map(|(binding, sessions)| sessions.remove(&session).then(|| binding.clone()))
    }
}

#[async_trait]
impl Admission for Slots {
    async fn admit(&self, session: Uuid, launcher: &dyn Launcher) -> bool {
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return false;
        };
        let binding = match self.binding_of(&mut conn, session).await {
            // No binding opened it: no binding's limit holds it back.
            Ok(None) => return true,
            Ok(Some(binding)) => binding,
            Err(error) => {
                logged(error);
                return false;
            }
        };
        let limits = match self.limits(&mut conn, &binding).await {
            Ok(limits) => limits,
            Err(error) => {
                logged(error);
                return false;
            }
        };
        drop(conn);
        // When this is `false`, the binding's other sessions hold every run
        // it may have: this one waits for one of them to end.
        self.take(launcher, &binding, session, limits.runs)
    }

    fn withdraw(&self, session: Uuid) {
        self.free(session);
    }

    async fn ended(&self, session: Uuid) -> Vec<Uuid> {
        // A run slot of that binding came free: its other waiting sessions
        // may take it.
        match self.free(session) {
            Some(binding) => self.waiting_of(&binding).await,
            None => Vec::new(),
        }
    }
}

impl Sessions {
    /// `ceilings` are the operator's, on every binding. The binding limits
    /// on runs take hold of `inbox`, which starts every session's runs.
    pub fn new(db: DbPool, stores: Stores, inbox: Arc<Inbox>, ceilings: Ceilings) -> Self {
        let slots = Arc::new(Slots {
            db: db.clone(),
            ceilings,
            running: Mutex::new(HashMap::new()),
        });
        inbox.govern(Arc::clone(&slots) as Arc<dyn Admission>);
        Sessions {
            db,
            stores,
            inbox,
            slots,
            sent: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one of `binding`'s messages for this minute, or refuses.
    fn take_message(&self, binding: &BindingKey, per_minute: Option<u32>) -> Option<Instant> {
        let now = Instant::now();
        // Unlimited: nothing to count, and `give_back` finds nothing to return.
        let Some(per_minute) = per_minute else {
            return Some(now);
        };
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
        let limits = self
            .slots
            .limits(&mut conn, &binding)
            .await
            .map_err(|error| {
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
                    if limits
                        .sessions
                        .is_some_and(|cap| live.count >= i64::from(cap))
                    {
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
        envelope: impl Into<Envelope>,
        dedupe: Option<&str>,
    ) -> Result<(), SendError> {
        let envelope = envelope.into();
        check_send(&envelope, dedupe)?;
        let session: Uuid = session.parse().map_err(|_| SendError::NotFound)?;
        let mut conn = self.db.get().await.map_err(|error| {
            logged(error);
            SendError::Unavailable
        })?;
        let limits = self
            .slots
            .limits(&mut conn, binding)
            .await
            .map_err(|error| {
                logged(error);
                SendError::Unavailable
            })?;
        let taken = self
            .take_message(binding, limits.messages_per_minute)
            .ok_or(SendError::Busy)?;
        let rate = binding.clone();
        let binding = binding.clone();
        let said = Said {
            text: envelope.message.text(),
            origin: Origin::plugin(&binding.plugin, envelope.from, envelope.reply_to),
        };
        let dedupe = dedupe.map(|id| Dedupe {
            scope: inbox::PLUGIN,
            id,
        });
        let inbox = &self.inbox;
        let accepted = conn
            .transaction::<bool, Failed<SendError>, _>(move |conn| {
                async move {
                    // Held across the ownership check, so an archive cannot
                    // land between it and the message being written.
                    inbox::lock(conn, session).await?;
                    match ownership(conn, &binding, session).await? {
                        Owner::Missing => return Err(Failed::Answer(SendError::NotFound)),
                        Owner::Other => return Err(Failed::Answer(SendError::NotOwned)),
                        Owner::Mine { archived: true } => {
                            return Err(Failed::Answer(SendError::Archived));
                        }
                        Owner::Mine { archived: false } => {}
                    }
                    // A refusal rolls the dedupe id back too: a refused
                    // message was never seen.
                    inbox
                        .accept(conn, session, &said, dedupe)
                        .await?
                        .map_err(|refused| {
                            Failed::Answer(match refused {
                                Refused::Missing => SendError::NotFound,
                                Refused::Closed => SendError::Archived,
                                Refused::Full => SendError::Busy,
                            })
                        })
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
            self.inbox.kick(session).await;
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
                inbox::lock(conn, session).await?;
                match ownership(conn, &binding, session).await? {
                    Owner::Missing => return Err(Failed::Answer(SessionError::NotFound)),
                    Owner::Other => return Err(Failed::Answer(SessionError::NotOwned)),
                    Owner::Mine { .. } => {}
                }
                // The agents it spawned are closed with it.
                crate::children::close(conn, session, crate::models::now_epoch()).await?;
                inbox::clear(conn, session).await?;
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
        envelope: Envelope,
        dedupe: Option<&str>,
    ) -> Result<(), SendError> {
        if self.lease.is_retired() {
            return Err(SendError::Gone);
        }
        self.sessions
            .send(&self.binding, session, envelope, dedupe)
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::inbox::testing::FakeLauncher;
    use crate::plugins::daemon::fixture::Fixture;
    use crate::plugins::daemon::storage::Limits;
    use diesel::sql_types::{Integer, Nullable};
    use plugin::Message;
    use plugin::daemon::{Author, ReplyTo};

    fn with(
        fixture: &Fixture,
        cap: usize,
        ceilings: Ceilings,
    ) -> (Arc<Sessions>, Arc<FakeLauncher>) {
        let inbox = Arc::new(Inbox::new(fixture.pool.clone()).with_cap(cap));
        let launcher = Arc::new(FakeLauncher::default());
        inbox.attach(launcher.clone());
        let sessions = Arc::new(Sessions::new(
            fixture.pool.clone(),
            Stores::new(fixture.pool.clone(), Limits::default()),
            inbox,
            ceilings,
        ));
        (sessions, launcher)
    }

    fn sessions(fixture: &Fixture) -> (Arc<Sessions>, Arc<FakeLauncher>) {
        with(fixture, 2, Ceilings::default())
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
    async fn a_run_takes_each_message_with_who_said_it() {
        let fixture = Fixture::new(0).await;
        let (sessions, launcher) = sessions(&fixture);
        let discord = fixture.binding("discord");
        let session = sessions.open(&discord, "thread:1", None).await.unwrap().id;
        let id: Uuid = session.parse().unwrap();
        let ana = Author::new("42", "Ana");
        let mut bo = Author::new("77", "Bo");
        bo.avatar = Some("https://cdn.example/bo.png".into());

        // The first starts a run; the other two wait for it, under the cap.
        let waits = [
            Envelope::from(Message::user("hello")).by(ana.clone()),
            Envelope::from(Message::user("hi Ana"))
                .by(bo.clone())
                .replying(ReplyTo::Author(ana.clone())),
            Envelope::from(Message::user("and you?"))
                .by(ana.clone())
                .replying(ReplyTo::Agent),
        ];
        for envelope in waits {
            sessions
                .send(&discord, &session, envelope, None)
                .await
                .unwrap();
        }
        launcher.end(id);
        sessions.inbox.run_ended(id).await;

        let said = |text: &str, from: Option<&Author>, reply_to: Option<ReplyTo>| Said {
            text: text.to_owned(),
            origin: Origin::plugin("discord", from.cloned(), reply_to),
        };
        assert_eq!(
            *launcher.said.lock().unwrap(),
            [
                said("hello", Some(&ana), None),
                said("hi Ana", Some(&bo), Some(ReplyTo::Author(ana.clone()))),
                said("and you?", Some(&ana), Some(ReplyTo::Agent)),
            ]
        );
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
        sessions.inbox.run_ended(id).await;
        assert_eq!(
            launcher.launched(),
            [(id, "first".to_owned()), (id, "second\n\nthird".to_owned())]
        );
        // Taken: the next run end finds nothing.
        launcher.end(id);
        sessions.inbox.run_ended(id).await;
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
        sessions.inbox.run_ended(busy.parse().unwrap()).await;
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
            .send(&session, Message::user("one").into(), None)
            .await
            .unwrap();
        assert!(launcher.launched().is_empty());

        // The owner fixes the binding; the next kick takes what waited.
        // (`recover` would kick every session waiting in the shared test
        // database, so this kicks its own.)
        launcher
            .refuse
            .store(false, std::sync::atomic::Ordering::SeqCst);
        sessions.inbox.run_ended(session.parse().unwrap()).await;
        assert_eq!(launcher.launched().len(), 1);
        assert_eq!(launcher.launched()[0].1, "one");

        lease.retire();
        assert_eq!(handle.open("thread:1", None).await, Err(SessionError::Gone));
        assert_eq!(
            handle.send(&session, Message::user("x").into(), None).await,
            Err(SendError::Gone)
        );
        assert_eq!(handle.archive(&session).await, Err(SessionError::Gone));
        fixture.cleanup().await;
    }

    fn limited(fixture: &Fixture, ceilings: Ceilings) -> (Arc<Sessions>, Arc<FakeLauncher>) {
        with(fixture, plugin::daemon::PENDING_CAP, ceilings)
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
        assert_eq!(ceilings.runs, Some(3));
        // Unset is no ceiling at all.
        assert_eq!(ceilings.sessions, None);
        assert_eq!(Ceilings::default(), Ceilings::from_vars(|_| None).unwrap());
        assert!(Ceilings::from_vars(|_| Some("0".into())).is_err());
        let held = ceilings.with(Some(rows::OwnerLimits {
            max_sessions: Some(5),
            max_runs: Some(10),
            messages_per_minute: None,
        }));
        assert_eq!((held.sessions, held.runs), (Some(5), Some(3)));
        assert_eq!(held.messages_per_minute, None);
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
                runs: Some(1),
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
        sessions.inbox.run_ended(first.parse().unwrap()).await;
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
                messages_per_minute: Some(2),
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
