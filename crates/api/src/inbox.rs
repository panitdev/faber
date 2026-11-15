//! A session's inbox: messages waiting for its next run, from any sender.
//!
//! A message is accepted once its row is written (`session_inbox`), in the
//! same transaction that records its dedupe id (`session_inbox_dedupe`). An
//! idle session starts a run with everything waiting on it; a session runs
//! one at a time, so a message put during a run waits for it to end and the
//! next run takes it.
//!
//! Each row carries who sent it, and that is all a sender decides. The run a
//! message starts is the session's own: its owner's credentials, and the
//! model its settings name.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use diesel::OptionalExtension;
use diesel::sql_types::{BigInt, Integer, Jsonb, Text, Uuid as SqlUuid};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use plugin::daemon::{DEDUPE_WINDOW, PENDING_CAP};
use uuid::Uuid;

use crate::db::DbPool;
use crate::said::{Author, Origin};

/// The dedupe scope of messages a plugin's binding sends.
pub const PLUGIN: &str = "plugin";

/// One waiting message, as its run takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    pub text: String,
    pub origin: Origin,
}

/// An id a sender gives a message so that sending it again is a no-op. Ids
/// are per session and per `scope`, the kind of sender, so one kind can
/// neither replay nor block another's.
#[derive(Clone, Copy, Debug)]
pub struct Dedupe<'a> {
    pub scope: &'a str,
    /// 1 to 128 printable ASCII characters.
    pub id: &'a str,
}

/// Why a message was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// No such session in any project.
    Missing,
    /// The session is closed.
    Closed,
    /// As many messages as may wait on one session already do.
    Full,
}

/// Why [`Inbox::put`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PutError {
    Refused(Refused),
    /// The database could not be reached; the message was not accepted.
    Unavailable,
}

/// Starts the run that takes a session's waiting messages. The host's is
/// [`StateLauncher`]; tests use a fake.
#[async_trait]
pub trait Launcher: Send + Sync + 'static {
    /// Whether a run of `session` is under way in this process.
    fn running(&self, session: Uuid) -> bool;

    /// Starts a run of `session` with `said` as the user's turn, in order.
    /// Returns once the run is claimed, so `running` answers `true` from then
    /// until it ends.
    async fn launch(&self, session: Uuid, said: Vec<Said>) -> Result<(), String>;
}

/// Decides whether a session with waiting messages may start a run now, for
/// limits on how many runs something has at once.
#[async_trait]
pub trait Admission: Send + Sync + 'static {
    /// Takes a place for a run of `session`, or answers `false` to leave its
    /// messages waiting.
    async fn admit(&self, session: Uuid, launcher: &dyn Launcher) -> bool;

    /// Gives back the place `admit` took, for a run that did not start.
    fn withdraw(&self, session: Uuid);

    /// A run of `session` ended. Returns the other sessions that may now be
    /// able to start one.
    async fn ended(&self, session: Uuid) -> Vec<Uuid>;
}

/// Rows of the raw queries below.
// The derive expands each field as `field: field`, which clippy flags.
#[allow(clippy::redundant_field_names)]
mod rows {
    use diesel::sql_types::{BigInt, Jsonb, Nullable, Text, Uuid as SqlUuid};
    use uuid::Uuid;

    #[derive(diesel::QueryableByName)]
    pub struct Session {
        #[diesel(sql_type = Nullable<SqlUuid>)]
        pub project_id: Option<Uuid>,
        #[diesel(sql_type = Nullable<BigInt>)]
        pub closed_at: Option<i64>,
    }

    #[derive(diesel::QueryableByName)]
    pub struct Waiting {
        #[diesel(sql_type = BigInt)]
        pub id: i64,
        #[diesel(sql_type = Text)]
        pub content: String,
        #[diesel(sql_type = Jsonb)]
        pub origin: serde_json::Value,
    }

    #[derive(diesel::QueryableByName)]
    pub struct Count {
        #[diesel(sql_type = BigInt)]
        pub count: i64,
    }

    #[derive(diesel::QueryableByName)]
    pub struct SessionId {
        #[diesel(sql_type = SqlUuid)]
        pub session_id: Uuid,
    }

    /// Whose run a session's is, and on what model.
    #[derive(diesel::QueryableByName)]
    pub struct RunsAs {
        #[diesel(sql_type = SqlUuid)]
        pub owner_id: Uuid,
        #[diesel(sql_type = Nullable<Text>)]
        pub model_alias: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        pub thinking_effort: Option<String>,
        /// The plugin whose binding opened the session, if one did.
        #[diesel(sql_type = Nullable<Text>)]
        pub plugin_type: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        pub binding_model: Option<String>,
        #[diesel(sql_type = Nullable<Text>)]
        pub binding_thinking: Option<String>,
    }
}

fn logged(error: impl std::fmt::Display) {
    tracing::warn!(%error, "session inbox call failed");
}

/// Holds `session`'s inbox for the rest of the transaction, so accepting a
/// message and closing the session cannot interleave.
pub async fn lock(
    conn: &mut AsyncPgConnection,
    session: Uuid,
) -> Result<(), diesel::result::Error> {
    diesel::sql_query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind::<Text, _>(format!("session_inbox:{session}"))
        .execute(conn)
        .await
        .map(|_| ())
}

/// Drops every message waiting on `session`, for one being closed.
pub async fn clear(
    conn: &mut AsyncPgConnection,
    session: Uuid,
) -> Result<(), diesel::result::Error> {
    diesel::sql_query("DELETE FROM session_inbox WHERE session_id = $1")
        .bind::<SqlUuid, _>(session)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Every session's inbox.
pub struct Inbox {
    db: DbPool,
    cap: usize,
    launcher: OnceLock<Arc<dyn Launcher>>,
    admission: OnceLock<Arc<dyn Admission>>,
    /// One gate per session with a kick under way, so two kicks never both
    /// see "idle" and start two runs.
    gates: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

impl Inbox {
    pub fn new(db: DbPool) -> Self {
        Inbox {
            db,
            cap: PENDING_CAP,
            launcher: OnceLock::new(),
            admission: OnceLock::new(),
            gates: Mutex::new(HashMap::new()),
        }
    }

    /// At most `cap` messages wait on one session.
    pub fn with_cap(mut self, cap: usize) -> Self {
        self.cap = cap;
        self
    }

    /// What starts runs. Set once, after the server state exists; until then
    /// accepted messages wait.
    pub fn attach(&self, launcher: Arc<dyn Launcher>) {
        let _ = self.launcher.set(launcher);
    }

    /// What may hold a session's run back. Set once.
    pub fn govern(&self, admission: Arc<dyn Admission>) {
        let _ = self.admission.set(admission);
    }

    /// Writes `said` to `session`'s inbox inside the caller's transaction,
    /// which must be rolled back on a refusal: the dedupe id of a refused
    /// message is not to be kept. `Ok(Ok(false))` is a replayed dedupe id,
    /// which changes nothing. The caller kicks the session once it commits.
    pub async fn accept(
        &self,
        conn: &mut AsyncPgConnection,
        session: Uuid,
        said: &Said,
        dedupe: Option<Dedupe<'_>>,
    ) -> Result<Result<bool, Refused>, diesel::result::Error> {
        lock(conn, session).await?;
        let found: Option<rows::Session> =
            diesel::sql_query("SELECT project_id, closed_at FROM session WHERE id = $1")
                .bind::<SqlUuid, _>(session)
                .get_result(conn)
                .await
                .optional()?;
        match found {
            // Outside a project there is no owner to run as.
            None
            | Some(rows::Session {
                project_id: None, ..
            }) => return Ok(Err(Refused::Missing)),
            Some(rows::Session {
                closed_at: Some(_), ..
            }) => return Ok(Err(Refused::Closed)),
            Some(_) => {}
        }
        if let Some(dedupe) = dedupe {
            diesel::sql_query(
                "DELETE FROM session_inbox_dedupe WHERE session_id = $1 \
                 AND created_at < now() - make_interval(secs => $2)",
            )
            .bind::<SqlUuid, _>(session)
            .bind::<Integer, _>(DEDUPE_WINDOW.as_secs() as i32)
            .execute(conn)
            .await?;
            let recorded = diesel::sql_query(
                "INSERT INTO session_inbox_dedupe (session_id, scope, dedupe_id) \
                 VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            )
            .bind::<SqlUuid, _>(session)
            .bind::<Text, _>(dedupe.scope)
            .bind::<Text, _>(dedupe.id)
            .execute(conn)
            .await?;
            if recorded == 0 {
                return Ok(Ok(false));
            }
        }
        let waiting: rows::Count =
            diesel::sql_query("SELECT count(*) AS count FROM session_inbox WHERE session_id = $1")
                .bind::<SqlUuid, _>(session)
                .get_result(conn)
                .await?;
        if waiting.count >= self.cap as i64 {
            return Ok(Err(Refused::Full));
        }
        // Cannot fail to encode: an origin is strings all the way down.
        let origin = serde_json::to_value(&said.origin).unwrap_or_default();
        diesel::sql_query(
            "INSERT INTO session_inbox (session_id, content, origin) VALUES ($1, $2, $3)",
        )
        .bind::<SqlUuid, _>(session)
        .bind::<Text, _>(&said.text)
        .bind::<Jsonb, _>(origin)
        .execute(conn)
        .await?;
        Ok(Ok(true))
    }

    /// Accepts `said` for `session` and returns once it is written. A dedupe
    /// id already recorded against the session makes this a successful no-op.
    /// An idle session starts its run now; a busy one takes the message when
    /// its run ends.
    // Its first caller is the agent tools; until then only tests put.
    #[allow(dead_code)]
    pub async fn put(
        &self,
        session: Uuid,
        said: Said,
        dedupe: Option<Dedupe<'_>>,
    ) -> Result<(), PutError> {
        enum Failed {
            Refused(Refused),
            Db(diesel::result::Error),
        }
        impl From<diesel::result::Error> for Failed {
            fn from(error: diesel::result::Error) -> Self {
                Failed::Db(error)
            }
        }
        let mut conn = self.db.get().await.map_err(|error| {
            logged(error);
            PutError::Unavailable
        })?;
        let accepted = conn
            .transaction::<bool, Failed, _>(|conn| {
                async move {
                    self.accept(conn, session, &said, dedupe)
                        .await?
                        .map_err(Failed::Refused)
                }
                .scope_boxed()
            })
            .await;
        drop(conn);
        match accepted {
            Ok(true) => self.kick(session).await,
            Ok(false) => {}
            Err(Failed::Refused(refused)) => return Err(PutError::Refused(refused)),
            Err(Failed::Db(error)) => {
                logged(error);
                return Err(PutError::Unavailable);
            }
        }
        Ok(())
    }

    /// A run of `session` ended: its next run takes what waits on it, and
    /// sessions that were held back behind it may start theirs.
    pub async fn run_ended(&self, session: Uuid) {
        let others = match self.admission.get() {
            Some(admission) => admission.ended(session).await,
            None => Vec::new(),
        };
        self.kick(session).await;
        for other in others {
            if other != session {
                self.kick(other).await;
            }
        }
    }

    /// Kicks every session with waiting messages: what a process that died
    /// before running them left behind. Called once at startup.
    pub async fn recover(&self) {
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return;
        };
        let waiting: Vec<rows::SessionId> =
            match diesel::sql_query("SELECT DISTINCT session_id FROM session_inbox")
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
    pub async fn kick(&self, session: Uuid) {
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
        // Nothing waits on a closed session: closing one clears its inbox,
        // and this leaves alone what a close in another process has not
        // cleared yet.
        let waiting: Vec<rows::Waiting> = match diesel::sql_query(
            "SELECT i.id, i.content, i.origin FROM session_inbox i \
             JOIN session s ON s.id = i.session_id \
             WHERE i.session_id = $1 AND s.closed_at IS NULL ORDER BY i.id",
        )
        .bind::<SqlUuid, _>(session)
        .load(&mut conn)
        .await
        {
            Ok(rows) => rows,
            Err(error) => return logged(error),
        };
        drop(conn);
        let Some(last) = waiting.last().map(|row| row.id) else {
            return;
        };
        let admission = self.admission.get();
        if let Some(admission) = admission
            && !admission.admit(session, launcher).await
        {
            return;
        }

        let said = waiting
            .into_iter()
            .map(|row| Said {
                text: row.content,
                // A column that no longer decodes loses the sender, never
                // the message.
                origin: serde_json::from_value(row.origin).unwrap_or_else(|_| Origin {
                    author: Author {
                        source: "unknown".to_owned(),
                        id: None,
                        name: None,
                        avatar: None,
                    },
                    reply_to: None,
                }),
            })
            .collect();
        if let Err(error) = launcher.launch(session, said).await {
            if let Some(admission) = admission {
                admission.withdraw(session);
            }
            // Left waiting: the session getting a model, or the next
            // message, kicks again.
            tracing::warn!(%session, %error, "inbox messages wait: no run started");
            return;
        }
        // The run has recorded them as its input. A crash before this line
        // delivers them twice; one after it, never — the run already has them.
        let Ok(mut conn) = self.db.get().await.map_err(logged) else {
            return;
        };
        if let Err(error) =
            diesel::sql_query("DELETE FROM session_inbox WHERE session_id = $1 AND id <= $2")
                .bind::<SqlUuid, _>(session)
                .bind::<BigInt, _>(last)
                .execute(&mut conn)
                .await
        {
            logged(error);
        }
    }
}

/// The host's [`Launcher`]: a run as the session's owner would start one. A
/// session a binding opened runs on the model the owner picked for that
/// binding; any other runs on the model the session itself remembers.
pub struct StateLauncher(pub crate::state::AppState);

#[async_trait]
impl Launcher for StateLauncher {
    fn running(&self, session: Uuid) -> bool {
        crate::run::is_running(&self.0.runs, session)
    }

    async fn launch(&self, session: Uuid, said: Vec<Said>) -> Result<(), String> {
        let state = &self.0;
        let mut conn = state.db.get().await.map_err(|error| error.to_string())?;
        let runs_as: rows::RunsAs = diesel::sql_query(
            "SELECT p.owner_id, s.model_alias, s.thinking_effort, b.plugin_type, \
                    pb.model_alias AS binding_model, pb.thinking_effort AS binding_thinking \
             FROM session s JOIN project p ON p.id = s.project_id \
             LEFT JOIN binding_session b ON b.session_id = s.id \
             LEFT JOIN project_binding pb \
               ON pb.project_id = b.project_id AND pb.plugin_type = b.plugin_type \
             WHERE s.id = $1",
        )
        .bind::<SqlUuid, _>(session)
        .get_result(&mut conn)
        .await
        .map_err(|error| format!("the session is not in a project: {error}"))?;
        let (model, thinking) = match runs_as.plugin_type {
            Some(_) => (
                runs_as
                    .binding_model
                    .ok_or("the owner has not picked a model for this binding")?,
                runs_as.binding_thinking,
            ),
            None => (
                runs_as
                    .model_alias
                    .ok_or("no model has been picked for this session")?,
                runs_as.thinking_effort,
            ),
        };
        let resolved = crate::resolve::resolve_model(state, runs_as.owner_id, &model)
            .await
            .map_err(|error| format!("model `{model}`: {error}"))?;
        let thinking = crate::models::thinking::ThinkingSelection::from_stored(thinking.as_deref());
        let thread = crate::routes::sessions::resolve_thread(&mut conn, session, None)
            .await
            .map_err(|error| error.to_string())?;
        // One user turn of several messages: they were all said before the
        // agent could answer any of them.
        let input = said
            .into_iter()
            .map(|said| crate::run::TurnMessage::said(said.text, said.origin))
            .collect();
        crate::routes::sessions::launch_run(
            state,
            conn,
            crate::routes::sessions::Launch {
                session_id: session,
                thread_id: thread,
                user_id: runs_as.owner_id,
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
pub(crate) mod testing {
    use std::collections::HashSet;

    use super::*;

    /// Records launches; a session is "running" until the test ends it.
    #[derive(Default)]
    pub struct FakeLauncher {
        running: Mutex<HashSet<Uuid>>,
        launched: Mutex<Vec<(Uuid, String)>>,
        pub said: Mutex<Vec<Said>>,
        pub refuse: std::sync::atomic::AtomicBool,
    }

    impl FakeLauncher {
        /// Each run started: its session and its messages, joined.
        pub fn launched(&self) -> Vec<(Uuid, String)> {
            self.launched.lock().unwrap().clone()
        }

        pub fn end(&self, session: Uuid) {
            self.running.lock().unwrap().remove(&session);
        }
    }

    #[async_trait]
    impl Launcher for FakeLauncher {
        fn running(&self, session: Uuid) -> bool {
            self.running.lock().unwrap().contains(&session)
        }

        async fn launch(&self, session: Uuid, said: Vec<Said>) -> Result<(), String> {
            // Spelled out: `load` on its own is the query method in scope.
            if std::sync::atomic::AtomicBool::load(
                &self.refuse,
                std::sync::atomic::Ordering::SeqCst,
            ) {
                return Err("no model".into());
            }
            self.running.lock().unwrap().insert(session);
            let text = said
                .iter()
                .map(|said| said.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            self.launched.lock().unwrap().push((session, text));
            self.said.lock().unwrap().extend(said);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeLauncher;
    use super::*;
    use crate::plugins::daemon::fixture::Fixture;

    fn inbox(fixture: &Fixture) -> (Inbox, Arc<FakeLauncher>) {
        let inbox = Inbox::new(fixture.pool.clone()).with_cap(2);
        let launcher = Arc::new(FakeLauncher::default());
        inbox.attach(launcher.clone());
        (inbox, launcher)
    }

    fn from_agent(text: &str) -> Said {
        Said {
            text: text.to_owned(),
            origin: Origin::agent(Uuid::nil(), Some("reviewer")),
        }
    }

    const AGENT: &str = "agent";

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_message_starts_a_run_of_an_idle_session_no_binding_opened() {
        let fixture = Fixture::new(1).await;
        let (inbox, launcher) = inbox(&fixture);
        let session = fixture.sessions[0];

        inbox.put(session, from_agent("first"), None).await.unwrap();
        assert_eq!(launcher.launched(), [(session, "first".to_owned())]);
        assert_eq!(*launcher.said.lock().unwrap(), [from_agent("first")]);

        // The run is under way: these wait, and its end starts the next.
        for text in ["second", "third"] {
            inbox.put(session, from_agent(text), None).await.unwrap();
        }
        assert_eq!(launcher.launched().len(), 1);
        launcher.end(session);
        inbox.run_ended(session).await;
        assert_eq!(
            launcher.launched()[1],
            (session, "second\n\nthird".to_owned())
        );
        // Taken: the next run end finds nothing.
        launcher.end(session);
        inbox.run_ended(session).await;
        assert_eq!(launcher.launched().len(), 2);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_replayed_dedupe_id_is_a_no_op_within_its_scope() {
        let fixture = Fixture::new(1).await;
        let (inbox, launcher) = inbox(&fixture);
        let session = fixture.sessions[0];
        let id = |scope| Some(Dedupe { scope, id: "m1" });

        inbox
            .put(session, from_agent("hello"), id(AGENT))
            .await
            .unwrap();
        launcher.end(session);
        inbox
            .put(session, from_agent("hello"), id(AGENT))
            .await
            .unwrap();
        assert_eq!(launcher.launched().len(), 1);
        // Another kind of sender's `m1` is another message.
        inbox
            .put(session, from_agent("theirs"), id(PLUGIN))
            .await
            .unwrap();
        assert_eq!(launcher.launched().len(), 2);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_full_closed_or_missing_inbox_refuses() {
        let fixture = Fixture::new(2).await;
        let (inbox, launcher) = inbox(&fixture);
        let [busy, closed] = fixture.sessions[..] else {
            unreachable!()
        };
        let m9 = Some(Dedupe {
            scope: AGENT,
            id: "m9",
        });

        inbox
            .put(busy, from_agent("starts a run"), None)
            .await
            .unwrap();
        for n in 0..2 {
            let said = from_agent(&format!("waits {n}"));
            inbox.put(busy, said, None).await.unwrap();
        }
        assert_eq!(
            inbox.put(busy, from_agent("one too many"), m9).await,
            Err(PutError::Refused(Refused::Full))
        );
        // The refused message's dedupe id was not kept: once there is room,
        // the same id goes through.
        launcher.end(busy);
        inbox.run_ended(busy).await;
        inbox
            .put(busy, from_agent("one too many"), m9)
            .await
            .unwrap();

        let mut conn = fixture.pool.get().await.unwrap();
        diesel::sql_query("UPDATE session SET closed_at = 1 WHERE id = $1")
            .bind::<SqlUuid, _>(closed)
            .execute(&mut conn)
            .await
            .unwrap();
        drop(conn);
        assert_eq!(
            inbox.put(closed, from_agent("late"), None).await,
            Err(PutError::Refused(Refused::Closed))
        );
        assert_eq!(
            inbox.put(Uuid::now_v7(), from_agent("x"), None).await,
            Err(PutError::Refused(Refused::Missing))
        );
        fixture.cleanup().await;
    }
}
