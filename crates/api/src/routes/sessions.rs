use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use diesel::{ExpressionMethods, JoinOnDsl, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::{AsyncConnection, RunQueryDsl, scoped_futures::ScopedFutureExt};
use futures::stream::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use uuid::Uuid;

use crate::{
    access::{authorize_session, authorize_workspace, personal_workspace},
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{
        now_epoch,
        run::NewRun,
        session::{NewSession, Session, UpdateSession},
        thinking::ThinkingSelection,
        thread::{NewThread, Thread},
        transcript::Transcript,
    },
    resolve::resolve_model,
    routes::{
        clamp_limit, deserialize_optional_field,
        threads::{ThreadResponse, thread_response},
    },
    run::{RunRequest, StreamEvent},
    schema::{models, run, session, thread, transcript, workspace_member},
    state::AppState,
};
// Aliased: `crate::run` (the runner) and `crate::schema::run` (the table) are
// both reached from this file.
use crate::run as runner;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/sessions", get(list).post(create))
        .route(
            "/api/sessions/{id}",
            get(get_session).patch(update).delete(remove),
        )
        .route(
            "/api/sessions/{id}/threads",
            get(list_threads).post(create_thread),
        )
        .route("/api/sessions/{id}/messages", post(send_message))
        .route("/api/sessions/{id}/stream", get(stream))
}

const MAX_TITLE_CHARS: usize = 200;

#[derive(Deserialize)]
struct ListQuery {
    workspace_id: Option<Uuid>,
    limit: Option<i64>,
}

/// Both fields optional: `POST /api/sessions` with `{}` is the intended default path.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateRequest {
    workspace_id: Option<Uuid>,
    title: Option<String>,
}

#[derive(Deserialize)]
struct UpdateRequest {
    /// `null` clears the title; omitting the key leaves it unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    title: Option<Option<String>>,
    /// `true` stamps `closed_at`; `false` reopens.
    closed: Option<bool>,
    /// The alias new messages go to. `null` clears the selection; omitting
    /// the key leaves it unchanged. This is where the model picker writes,
    /// so the choice survives a reload without anything having been sent.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    model: Option<Option<String>>,
    /// `"off"`, `"on"`, or an effort level. `null` clears the selection back
    /// to whatever the model's own definition defaults to — which is a
    /// different state from `"off"`.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    thinking_effort: Option<Option<String>>,
}

#[derive(Deserialize)]
struct CreateThreadRequest {
    /// Fork source. Must be given together with `forked_at_seq` — the database `CHECK`
    /// requires both or neither.
    parent_id: Option<Uuid>,
    forked_at_seq: Option<i32>,
}

#[derive(Serialize)]
struct SessionResponse {
    id: Uuid,
    workspace_id: Uuid,
    title: Option<String>,
    created_at: i64,
    closed_at: Option<i64>,
    /// The model alias new messages go to, or `null` if nothing picked one
    /// yet. Named `model` rather than `model_alias` to match what
    /// `POST /messages` already calls it.
    model: Option<String>,
    /// The thinking knob as the user left it, or `null` for the model's own
    /// default.
    thinking_effort: Option<String>,
}

fn session_response(s: &Session) -> SessionResponse {
    SessionResponse {
        id: s.id,
        workspace_id: s.workspace_id,
        title: s.title.clone(),
        created_at: s.created_at,
        closed_at: s.closed_at,
        model: s.model_alias.clone(),
        thinking_effort: s.thinking_effort.clone(),
    }
}

/// A new session always arrives with its root thread — a session with no thread is not a
/// state any caller has a use for.
#[derive(Serialize)]
struct CreatedSessionResponse {
    #[serde(flatten)]
    session: SessionResponse,
    root_thread: ThreadResponse,
}

fn validate_title(title: &str) -> Result<(), AppError> {
    if title.is_empty() {
        return Err(AppError::BadRequest("title cannot be empty".into()));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(AppError::BadRequest(format!(
            "title must be {MAX_TITLE_CHARS} characters or fewer"
        )));
    }
    Ok(())
}

/// Sessions across every workspace the caller belongs to, newest first, optionally
/// narrowed to one workspace.
async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(params): Query<ListQuery>,
) -> ApiResult<Json<Vec<SessionResponse>>> {
    let mut conn = state.db.get().await?;

    if let Some(workspace_id) = params.workspace_id {
        authorize_workspace(&mut conn, user.id, workspace_id).await?;
    }

    let mut query = session::table
        .inner_join(
            workspace_member::table.on(workspace_member::workspace_id.eq(session::workspace_id)),
        )
        .filter(workspace_member::user_id.eq(user.id))
        .into_boxed();

    if let Some(workspace_id) = params.workspace_id {
        query = query.filter(session::workspace_id.eq(workspace_id));
    }

    let rows: Vec<Session> = query
        .order(session::created_at.desc())
        .limit(clamp_limit(params.limit))
        .select(Session::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.list"))?;

    Ok(Json(rows.iter().map(session_response).collect()))
}

/// Creates a session and its root thread atomically. With no body fields the session
/// lands in the caller's personal workspace and is untitled.
async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    body: Option<Json<CreateRequest>>,
) -> ApiResult<(StatusCode, Json<CreatedSessionResponse>)> {
    let input = body.map(|Json(input)| input).unwrap_or_default();

    let title = input.title.as_deref().map(str::trim);
    if let Some(title) = title {
        validate_title(title)?;
    }

    let mut conn = state.db.get().await?;

    let workspace = match input.workspace_id {
        Some(id) => authorize_workspace(&mut conn, user.id, id).await?,
        None => personal_workspace(&mut conn, user.id).await?,
    };

    let now = now_epoch();
    let session_id = Uuid::now_v7();
    let thread_id = Uuid::now_v7();

    let (created_session, root_thread) = conn
        .transaction::<_, AppError, _>(|conn| {
            async move {
                let created_session: Session = diesel::insert_into(session::table)
                    .values(&NewSession {
                        id: session_id,
                        workspace_id: workspace.id,
                        title,
                        created_at: now,
                    })
                    .returning(Session::as_returning())
                    .get_result(conn)
                    .await
                    .map_err(|err| AppError::db(err, "sessions.create.insert_session"))?;

                let root_thread: Thread = diesel::insert_into(thread::table)
                    .values(&NewThread {
                        id: thread_id,
                        session_id,
                        parent_id: None,
                        forked_at_seq: None,
                        created_at: now,
                    })
                    .returning(Thread::as_returning())
                    .get_result(conn)
                    .await
                    .map_err(|err| AppError::db(err, "sessions.create.insert_root_thread"))?;

                Ok((created_session, root_thread))
            }
            .scope_boxed()
        })
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedSessionResponse {
            session: session_response(&created_session),
            root_thread: thread_response(&root_thread),
        }),
    ))
}

async fn get_session(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SessionResponse>> {
    let mut conn = state.db.get().await?;
    let found = authorize_session(&mut conn, user.id, id).await?;
    Ok(Json(session_response(&found)))
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRequest>,
) -> ApiResult<Json<SessionResponse>> {
    let title = input
        .title
        .as_ref()
        .map(|opt| opt.as_deref().map(str::trim));

    if let Some(Some(title)) = title {
        validate_title(title)?;
    }

    let model = input
        .model
        .as_ref()
        .map(|opt| opt.as_deref().map(str::trim));
    let thinking = input
        .thinking_effort
        .as_ref()
        .map(|opt| opt.as_deref().map(str::trim));

    // Refused rather than stored: the picker writes here the moment the user
    // chooses, and a selection that only fails when they next send a message
    // is a selection they will read as having been made.
    if let Some(Some(value)) = thinking {
        ThinkingSelection::parse(value).map_err(AppError::BadRequest)?;
    }

    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    if let Some(Some(alias)) = model {
        verify_model_alias(&mut conn, user.id, alias).await?;
    }

    let patch = UpdateSession {
        title,
        closed_at: input.closed.map(|closed| closed.then(now_epoch)),
        model_alias: model,
        thinking_effort: thinking,
    };

    let updated: Session = diesel::update(session::table.filter(session::id.eq(id)))
        .set(patch)
        .returning(Session::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::NotFound => AppError::NotFound,
            other => AppError::db(other, "sessions.update"),
        })?;

    Ok(Json(session_response(&updated)))
}

/// Deletes the session and, by cascade, every thread, run, exchange, and transcript row
/// under it — including ground truth that is otherwise append-only. The append-only
/// triggers permit this because they test whether the *parent* row still exists, and in a
/// cascade it does not. `PATCH { "closed": true }` is the reversible alternative.
async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    let deleted = diesel::delete(session::table.filter(session::id.eq(id)))
        .execute(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn list_threads(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<ThreadResponse>>> {
    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    let rows: Vec<Thread> = thread::table
        .filter(thread::session_id.eq(id))
        .order(thread::created_at.asc())
        .select(Thread::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.list_threads"))?;

    Ok(Json(rows.iter().map(thread_response).collect()))
}

/// Creates a thread in the session — a root thread by default, or a fork when
/// `parent_id` and `forked_at_seq` are both supplied.
async fn create_thread(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<CreateThreadRequest>>,
) -> ApiResult<(StatusCode, Json<ThreadResponse>)> {
    let input = body
        .map(|Json(input)| input)
        .unwrap_or(CreateThreadRequest {
            parent_id: None,
            forked_at_seq: None,
        });

    // Rejected here rather than at the database `CHECK` so the caller gets a 400 naming
    // the problem instead of a 500 naming a constraint.
    if input.parent_id.is_some() != input.forked_at_seq.is_some() {
        return Err(AppError::BadRequest(
            "parent_id and forked_at_seq must be given together".into(),
        ));
    }

    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    if let (Some(parent_id), Some(forked_at_seq)) = (input.parent_id, input.forked_at_seq) {
        let parent: Thread = thread::table
            .filter(thread::id.eq(parent_id))
            .filter(thread::session_id.eq(id))
            .select(Thread::as_select())
            .first(&mut conn)
            .await
            .map_err(|err| match err {
                diesel::result::Error::NotFound => {
                    AppError::BadRequest("parent thread not found in this session".into())
                }
                other => AppError::db(other, "sessions.create_thread.load_parent"),
            })?;

        // `forked_at_seq` is inclusive, so it must name a position the parent has
        // actually allocated.
        if parent.next_seq == 0 {
            return Err(AppError::BadRequest(
                "parent thread has no history to fork from".into(),
            ));
        }
        if forked_at_seq < 0 || forked_at_seq >= parent.next_seq {
            return Err(AppError::BadRequest(format!(
                "forked_at_seq must be between 0 and {}",
                parent.next_seq - 1
            )));
        }
    }

    let created: Thread = diesel::insert_into(thread::table)
        .values(&NewThread {
            id: Uuid::now_v7(),
            session_id: id,
            parent_id: input.parent_id,
            forked_at_seq: input.forked_at_seq,
            created_at: now_epoch(),
        })
        .returning(Thread::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.create_thread"))?;

    Ok((StatusCode::CREATED, Json(thread_response(&created))))
}

// ---------------------------------------------------------------------------
// Messages and the live stream
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SendMessageRequest {
    content: String,
    /// A model alias the caller owns (`faber -m fast`), not a provider model
    /// id. `a.md`: the alias is what the user types; `wire_id` is what goes in
    /// the request body.
    ///
    /// Optional, and remembered: the session carries the last alias it ran
    /// with, so a client that names one here is both choosing for this message
    /// and settling the question for the next. Omitting it runs on what the
    /// session already had, and a session that has nothing yet is a 400 rather
    /// than a guess.
    model: Option<String>,
    /// The thinking knob, remembered the same way — `"off"`, `"on"`, or an
    /// effort level. This is the first-message path: a client that has a
    /// selection before the session exists sends it here rather than making a
    /// second call to set it.
    thinking_effort: Option<String>,
    /// Which thread to run in. Optional while a session has exactly one.
    thread_id: Option<Uuid>,
}

#[derive(Serialize)]
struct SendMessageResponse {
    run_id: Uuid,
    thread_id: Uuid,
    /// Environments this message added to the session, in the order they were
    /// tagged. Empty when it tagged none, or only ones already bound.
    added_environments: Vec<String>,
}

#[derive(Deserialize)]
struct StreamQuery {
    /// Resume cursor, together with `after_seq`: replay that run's events past
    /// `after_seq` before switching to live. `transcript.seq` is unique per
    /// run, not per session, so neither half means anything alone.
    run_id: Option<Uuid>,
    after_seq: Option<i64>,
}

/// Starts a harness run against the session and returns immediately.
///
/// The run is detached: it outlives this response and every subscriber, and is
/// observed through `GET /api/sessions/{id}/stream` (live) or
/// `GET /api/runs/{id}/transcript` (durable).
async fn send_message(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<SendMessageRequest>,
) -> ApiResult<(StatusCode, Json<SendMessageResponse>)> {
    let content = input.content.trim();
    if content.is_empty() {
        return Err(AppError::BadRequest("content cannot be empty".into()));
    }

    let mut conn = state.db.get().await?;
    let found = authorize_session(&mut conn, user.id, id).await?;

    // A closed session is the reversible half of delete; writing to one would
    // make "closed" mean nothing.
    if found.closed_at.is_some() {
        return Err(AppError::BadRequest(
            "session is closed; reopen it with PATCH { \"closed\": false }".into(),
        ));
    }

    let thread_id = resolve_thread(&mut conn, id, input.thread_id).await?;

    // What this message runs with, and — because both are the session's own
    // settings rather than this message's — what the session is left holding
    // afterwards.
    let alias = match input.model.as_deref().map(str::trim) {
        Some(alias) if !alias.is_empty() => alias.to_owned(),
        _ => found.model_alias.clone().ok_or_else(|| {
            AppError::BadRequest(
                "no model is selected for this session; send \"model\" with the message, or set one with PATCH".into(),
            )
        })?,
    };

    let thinking = match input.thinking_effort.as_deref().map(str::trim) {
        Some(value) => Some(ThinkingSelection::parse(value).map_err(AppError::BadRequest)?),
        None => ThinkingSelection::from_stored(found.thinking_effort.as_deref()),
    };

    let resolved = resolve_model(&state, user.id, &alias).await?;

    // Written before the run starts, so a reload mid-run already shows what
    // the run is using. Only the fields this message actually named — a
    // message that carried no selection must not clear the one the session
    // has.
    let remembered = UpdateSession {
        model_alias: input.model.is_some().then_some(Some(alias.as_str())),
        thinking_effort: input
            .thinking_effort
            .is_some()
            .then_some(thinking.map(ThinkingSelection::as_str)),
        ..Default::default()
    };
    if remembered.model_alias.is_some() || remembered.thinking_effort.is_some() {
        diesel::update(session::table.filter(session::id.eq(id)))
            .set(remembered)
            .execute(&mut conn)
            .await
            .map_err(|err| AppError::db(err, "sessions.send_message.remember_selection"))?;
    }

    let run_id = Uuid::now_v7();
    diesel::insert_into(run::table)
        .values(&NewRun {
            id: run_id,
            thread_id,
            created_at: now_epoch(),
        })
        .execute(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.send_message.insert_run"))?;

    // Environments the message tagged. Adding is the user's — this is the
    // only path that binds one, and it binds what they named rather than what
    // the model asked for.
    let added = crate::environments::tag_environments(&mut conn, user.id, id, content).await?;

    let mut input = vec![runner::TurnMessage {
        kind: runner::KIND_INPUT,
        message: llm::Message {
            role: llm::Role::User,
            content: vec![llm::ContentBlock::Text {
                text: content.to_owned(),
            }],
        },
    }];

    // After the user's turn, never before it, and never in the system prompt.
    // The prompt sits ahead of every cached byte, and a leading system message
    // is hoisted into it by at least one provider's wire format — which would
    // turn a note about turn nine into part of the prefix every earlier turn
    // was cached against.
    if let Some(announcement) = crate::environments::announcement(&added) {
        input.push(runner::TurnMessage {
            kind: runner::KIND_ENVIRONMENTS,
            message: announcement,
        });
    }

    // Published on the same channel a subscriber is already holding, so the
    // user's own turn arrives in band rather than only on the next refetch.
    let input_events = runner::record_input(&mut conn, run_id, &input).await?;
    drop(conn);

    // Both claimed before the response goes out, so a client that acts on the
    // run id the moment it has one — opening the stream, or pressing stop —
    // cannot arrive before there is something there to reach.
    let sender = runner::open_run(&state.runs, id);
    let interrupt = runner::open_interrupt(&state.interrupts, run_id);
    for event in input_events {
        let _ = sender.send(event);
    }

    runner::spawn_run(
        state,
        sender.clone(),
        RunRequest {
            run_id,
            session_id: id,
            thread_id,
            user_id: user.id,
            config: resolved.config,
            api_key: resolved.api_key,
            thinking,
            input,
            interrupt,
        },
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(SendMessageResponse {
            run_id,
            thread_id,
            added_environments: added,
        }),
    ))
}

/// Checks that an alias names a model the caller owns.
///
/// The selection is stored as text — deliberately, so it means the same thing
/// to every member of a shared workspace — which leaves nothing in the schema
/// to stop a session pointing at a model that isn't there. Refusing at the
/// write is what keeps "picked" and "will run" the same answer at the moment
/// the user picks, rather than one message later.
async fn verify_model_alias(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    alias: &str,
) -> ApiResult<()> {
    let found: Option<Uuid> = models::table
        .filter(models::user_id.eq(user_id))
        .filter(models::alias.eq(alias))
        .select(models::id)
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "sessions.verify_model_alias"))?;

    match found {
        Some(_) => Ok(()),
        None => Err(AppError::BadRequest(format!("no model named '{alias}'"))),
    }
}

/// Picks the thread a message runs in.
///
/// A named thread is verified to belong to this session — the same check
/// `create_thread` makes of a fork parent, for the same reason: a thread id is
/// not a capability, membership in the session is.
async fn resolve_thread(
    conn: &mut diesel_async::AsyncPgConnection,
    session_id: Uuid,
    requested: Option<Uuid>,
) -> ApiResult<Uuid> {
    if let Some(thread_id) = requested {
        let found: Thread = thread::table
            .filter(thread::id.eq(thread_id))
            .filter(thread::session_id.eq(session_id))
            .select(Thread::as_select())
            .first(conn)
            .await
            .map_err(|err| match err {
                diesel::result::Error::NotFound => {
                    AppError::BadRequest("thread not found in this session".into())
                }
                other => AppError::db(other, "sessions.resolve_thread.load"),
            })?;
        return Ok(found.id);
    }

    let mut candidates: Vec<Thread> = thread::table
        .filter(thread::session_id.eq(session_id))
        .order(thread::created_at.asc())
        .limit(2)
        .select(Thread::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "sessions.resolve_thread.list"))?;

    match candidates.len() {
        0 => Err(AppError::BadRequest(
            "session has no thread to run in".into(),
        )),
        1 => Ok(candidates.remove(0).id),
        // Guessing here would silently split a conversation across branches.
        _ => Err(AppError::BadRequest(
            "session has more than one thread; name one with thread_id".into(),
        )),
    }
}

/// Everything happening in the session, as it happens.
///
/// Subscription is taken **before** the replay query so nothing can land in
/// the gap between the two; anything the replay already covered is then
/// dropped from the live side by `(run_id, seq)`.
async fn stream(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<StreamQuery>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>> {
    let cursor = match (params.run_id, params.after_seq) {
        (Some(run_id), Some(after_seq)) => Some((run_id, after_seq)),
        (None, None) => None,
        _ => {
            return Err(AppError::BadRequest(
                "run_id and after_seq must be given together".into(),
            ));
        }
    };

    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    let live = runner::subscribe(&state.runs, id);

    let replay: Vec<StreamEvent> = match cursor {
        Some((run_id, after_seq)) => {
            // Scoped to the session by the join, so a cursor naming someone
            // else's run replays nothing rather than leaking it.
            let rows: Vec<Transcript> = transcript::table
                .inner_join(run::table.on(run::id.eq(transcript::run_id)))
                .inner_join(thread::table.on(thread::id.eq(run::thread_id)))
                .filter(transcript::run_id.eq(run_id))
                .filter(thread::session_id.eq(id))
                .filter(transcript::seq.gt(after_seq))
                .order(transcript::seq.asc())
                .limit(clamp_limit(None))
                .select(Transcript::as_select())
                .load(&mut conn)
                .await
                .map_err(|err| AppError::db(err, "sessions.stream.replay"))?;

            rows.into_iter()
                .map(|row| StreamEvent {
                    run_id,
                    seq: row.seq,
                    kind: row.kind,
                    payload: row.payload,
                })
                .collect()
        }
        None => Vec::new(),
    };
    drop(conn);

    let replayed_through = cursor.map(|(run_id, _)| {
        (
            run_id,
            replay.last().map(|event| event.seq).unwrap_or(i64::MIN),
        )
    });

    let live = BroadcastStream::new(live).filter_map(move |item| {
        futures::future::ready(match item {
            Ok(event) => {
                // Already delivered by the replay above.
                if let Some((run_id, through)) = replayed_through
                    && event.run_id == run_id
                    && event.seq <= through
                {
                    return futures::future::ready(None);
                }
                Some(sse_event(&event))
            }
            // The subscriber fell behind far enough that events were dropped.
            // Reported rather than swallowed, and the stream stays open: the
            // client re-syncs through the durable transcript endpoint.
            Err(BroadcastStreamRecvError::Lagged(count)) => {
                Some(Ok(Event::default().event("lagged").data(count.to_string())))
            }
        })
    });

    let stream =
        futures::stream::iter(replay.iter().map(sse_event).collect::<Vec<_>>()).chain(live);

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn sse_event(event: &StreamEvent) -> Result<Event, std::convert::Infallible> {
    Ok(Event::default()
        .event("transcript")
        .data(serde_json::to_string(event).unwrap_or_else(|_| "{}".to_owned())))
}
