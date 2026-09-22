use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, JoinOnDsl, OptionalExtension, QueryDsl,
    SelectableHelper,
};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    access::{authorize_exchange, authorize_run},
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{
        exchange::Exchange, now_epoch, run::NewRun, session::Session, thinking::ThinkingSelection,
        transcript::Transcript,
    },
    resolve::resolve_model,
    routes::clamp_limit,
    run as runner,
    schema::{exchange, run, session, thread, transcript},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/runs/{id}/transcript", get(list_transcript))
        .route("/api/runs/{id}/exchanges", get(list_exchanges))
        .route("/api/exchanges/{id}", get(get_exchange))
        .route("/api/runs/{id}/interrupt", post(interrupt))
        .route("/api/runs/{id}/retry", post(retry))
}

#[derive(Deserialize)]
struct TranscriptQuery {
    /// Return only events strictly after this `seq`, so a client can fetch the tail
    /// without refetching the run. This is the durable record behind
    /// `GET /api/sessions/{id}/stream`, and where a subscriber that fell behind
    /// re-syncs from.
    after_seq: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
struct TranscriptResponse {
    id: Uuid,
    seq: i64,
    /// Free-form event tag. Deliberately not an enum in the database
    /// (`history-abstract.md` H8.7) so a new variant is not a migration.
    kind: String,
    payload: Value,
    created_at: i64,
}

fn transcript_response(t: &Transcript) -> TranscriptResponse {
    TranscriptResponse {
        id: t.id,
        seq: t.seq,
        kind: t.kind.clone(),
        payload: t.payload.clone(),
        created_at: t.created_at,
    }
}

/// What the user saw, in order — the harness-yielded event stream, not the provider
/// exchange. The two are separate logs and neither derives the other
/// (`history-abstract.md` H2); ground truth is not exported here.
async fn list_transcript(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<TranscriptQuery>,
) -> ApiResult<Json<Vec<TranscriptResponse>>> {
    let mut conn = state.db.get().await?;
    authorize_run(&mut conn, user.id, id).await?;

    let mut query = transcript::table
        .filter(transcript::run_id.eq(id))
        .into_boxed();

    if let Some(after_seq) = params.after_seq {
        query = query.filter(transcript::seq.gt(after_seq));
    }

    let rows: Vec<Transcript> = query
        .order(transcript::seq.asc())
        .limit(clamp_limit(params.limit))
        .select(Transcript::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "runs.list_transcript"))?;

    Ok(Json(rows.iter().map(transcript_response).collect()))
}

#[derive(Serialize)]
struct ExchangeResponse {
    id: Uuid,
    run_id: Uuid,
    /// Provider-reported token accounting, or `null` when it reported none.
    usage: Option<Value>,
    /// How the call ended — `{ "type": "ok" }` or a failure with its error.
    outcome: Option<Value>,
    expected_cache_tokens: i64,
    actual_cache_tokens: Option<i64>,
    /// Whether the provider event stream was recorded. Absent when it failed to
    /// encode at write time.
    has_provider_events: bool,
    /// True on the exchange a `spine` row names — the committed lineage. The
    /// rest are the garbage class (`history-abstract.md` H7).
    canonical: bool,
    started_at: i64,
    completed_at: Option<i64>,
}

fn exchange_response(exchange: &Exchange) -> ExchangeResponse {
    ExchangeResponse {
        id: exchange.id,
        run_id: exchange.run_id,
        usage: exchange.usage.clone(),
        outcome: exchange.outcome.clone(),
        expected_cache_tokens: exchange.expected_cache_tokens,
        actual_cache_tokens: exchange.actual_cache_tokens,
        has_provider_events: exchange.provider_events_digest.is_some(),
        canonical: exchange.canonical_blob_digest.is_some(),
        started_at: exchange.started_at,
        completed_at: exchange.completed_at,
    }
}

#[derive(Serialize)]
struct ExchangeDetailResponse {
    #[serde(flatten)]
    exchange: ExchangeResponse,
    /// The request bytes as sent, decoded as UTF-8 with replacement characters.
    /// Providers are sent JSON, so this is usually a JSON document.
    request: String,
    /// The provider event stream as received, parsed when it is JSON.
    provider_events: Option<Value>,
    /// The canonical lineage this exchange committed — present only on the
    /// committed exchange, and only when it produced one.
    canonical_blob: Option<Value>,
}

/// Decodes a stored blob to JSON, falling back to its text when it does not
/// parse. A debug surface should show the bytes it has rather than hide them
/// behind a parse error.
fn decode_json(bytes: Option<Vec<u8>>) -> Option<Value> {
    let bytes = bytes?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => Some(value),
        Err(_) => Some(Value::String(String::from_utf8_lossy(&bytes).into_owned())),
    }
}

/// What Core recorded at the capability boundary: the request and the provider
/// events, in call order. Ground truth, as opposed to the transcript's record
/// of what the user saw — the two are separate logs (`history-abstract.md` H2).
async fn list_exchanges(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<ExchangeResponse>>> {
    let mut conn = state.db.get().await?;
    authorize_run(&mut conn, user.id, id).await?;

    // `started_at` is the run's own start, shared by every exchange in the run,
    // so the v7 id — minted in call order — is what actually orders them.
    let rows: Vec<Exchange> = exchange::table
        .filter(exchange::run_id.eq(id))
        .order((exchange::started_at.asc(), exchange::id.asc()))
        .select(Exchange::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "runs.list_exchanges"))?;

    Ok(Json(rows.iter().map(exchange_response).collect()))
}

/// One exchange with the bytes behind its digests, for the debug viewer.
///
/// Kept off the list route on purpose: a request blob carries the whole context
/// a call sent, which is orders of magnitude larger than the metadata the list
/// needs.
async fn get_exchange(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExchangeDetailResponse>> {
    let mut conn = state.db.get().await?;
    let exchange = authorize_exchange(&mut conn, user.id, id).await?;

    let request = crate::blobs::read_blob(&mut conn, &exchange.request_blob_digest)
        .await?
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();

    let provider_events = match exchange.provider_events_digest.as_deref() {
        Some(digest) => decode_json(crate::blobs::read_blob(&mut conn, digest).await?),
        None => None,
    };

    let canonical_blob = match exchange.canonical_blob_digest.as_deref() {
        Some(digest) => decode_json(crate::blobs::read_blob(&mut conn, digest).await?),
        None => None,
    };

    Ok(Json(ExchangeDetailResponse {
        exchange: exchange_response(&exchange),
        request,
        provider_events,
        canonical_blob,
    }))
}

/// Asks a run in progress to stop, and returns as soon as the ask has landed.
///
/// Deliberately not a promise that the run has ended by the time this
/// responds. Stopping is a signal the harness meets as an ordinary failure of
/// its next model call or tool invocation (`abstract.md`: "cancellation
/// surfaces as an ordinary failure the harness has to deal with"), so a harness
/// that wants to commit what it has, or say one last thing, gets to — and a
/// harness that ignores the signal entirely is killed once the grace period is
/// out. Either way the end of the run is announced where every other end is,
/// on `GET /api/sessions/{id}/stream`, as `run_interrupted`.
///
/// Sending it twice is not an error: the flag is a state, not an edge, and a
/// user pressing stop again because nothing visibly happened yet is asking for
/// something reasonable.
async fn interrupt(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    authorize_run(&mut conn, user.id, id).await?;

    if runner::raise_interrupt(&state.interrupts, id) {
        return Ok(StatusCode::ACCEPTED);
    }

    // Re-read rather than trusting the row loaded a moment ago: a run that
    // finished between the two is the common way to reach here, and the whole
    // job left is telling "already over" apart from "not ours", which the
    // stale row would get wrong in exactly that case.
    let completed_at: Option<i64> = run::table
        .filter(run::id.eq(id))
        .select(run::completed_at)
        .first(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::NotFound => AppError::NotFound,
            other => AppError::db(other, "runs.interrupt.reload"),
        })?;

    if completed_at.is_some() {
        return Err(AppError::Conflict("run has already finished".into()));
    }

    // Running, but not here. The registry is in-process (`run.rs`), so an
    // instance that does not own the run has no way to reach it — reported
    // rather than answered with a misleading 202, which would tell the user
    // their run is stopping when nothing received the ask.
    tracing::warn!(run_id = %id, "interrupt for a run this instance does not own");
    Err(AppError::ServiceUnavailable(
        "run is not in progress on this instance".into(),
    ))
}

// ---------------------------------------------------------------------------
// Retry
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RetryRequest {
    /// `"full"` re-sends the original user input. `"from_checkpoint"` replays
    /// completed tool exchanges from the failed run so the model resumes where
    /// it left off.
    #[serde(default = "default_retry_mode")]
    mode: String,
}

fn default_retry_mode() -> String {
    "full".to_owned()
}

#[derive(Serialize)]
struct RetryResponse {
    run_id: Uuid,
    thread_id: Uuid,
    mode: String,
}

/// Retries a completed run that ended in error.
///
/// Two modes:
/// - `"full"`: re-sends the original user input as a new run. The seed has
///   not advanced (failed runs don't commit), so the model sees the same
///   history.
/// - `"from_checkpoint"`: reconstructs the conversation from the failed run's
///   transcript, including completed assistant messages and tool results, so
///   the model resumes from the last successful exchange.
async fn retry(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<RetryRequest>>,
) -> ApiResult<(StatusCode, Json<RetryResponse>)> {
    let mode = body
        .map(|Json(b)| b.mode)
        .unwrap_or_else(default_retry_mode);

    if mode != "full" && mode != "from_checkpoint" {
        return Err(AppError::BadRequest(
            "mode must be \"full\" or \"from_checkpoint\"".into(),
        ));
    }

    let mut conn = state.db.get().await?;
    let original = authorize_run(&mut conn, user.id, id).await?;

    if original.completed_at.is_none() {
        return Err(AppError::Conflict(
            "run is still in progress; interrupt it first".into(),
        ));
    }

    // Check the run ended in error — only error or interrupted runs are
    // retryable (a successful run has nothing to retry).
    let terminal: Option<Transcript> = transcript::table
        .filter(transcript::run_id.eq(id))
        .filter(
            transcript::kind
                .eq(runner::KIND_RUN_ERROR)
                .or(transcript::kind.eq(runner::KIND_RUN_INTERRUPTED)),
        )
        .select(Transcript::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "runs.retry.check_terminal"))?;

    if terminal.is_none() {
        return Err(AppError::BadRequest(
            "only failed or interrupted runs can be retried".into(),
        ));
    }

    let thread_id = original.thread_id;

    // Load the session to get the model alias.
    let found_session: Session = session::table
        .inner_join(thread::table.on(thread::session_id.eq(session::id)))
        .filter(thread::id.eq(thread_id))
        .select(Session::as_select())
        .first(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "runs.retry.load_session"))?;

    let session_id = found_session.id;

    if found_session.closed_at.is_some() {
        return Err(AppError::BadRequest("session is closed".into()));
    }

    let alias = found_session.model_alias.ok_or_else(|| {
        AppError::BadRequest("no model is selected for this session".into())
    })?;
    let thinking = ThinkingSelection::from_stored(found_session.thinking_effort.as_deref());

    let resolved = resolve_model(&state, user.id, &alias).await?;

    // Reconstruct the input from the original run's transcript.
    let rows: Vec<Transcript> = transcript::table
        .filter(transcript::run_id.eq(id))
        .order(transcript::seq.asc())
        .select(Transcript::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "runs.retry.load_transcript"))?;

    let mut input: Vec<runner::TurnMessage> = Vec::new();

    if mode == "full" {
        // Replay only the original user input and environment announcements.
        for row in &rows {
            if row.kind == runner::KIND_INPUT || row.kind == runner::KIND_ENVIRONMENTS {
                let message = transcript_to_message(&row.payload)?;
                input.push(runner::TurnMessage {
                    kind: if row.kind == runner::KIND_INPUT {
                        runner::KIND_INPUT
                    } else {
                        runner::KIND_ENVIRONMENTS
                    },
                    message,
                });
            }
        }
    } else {
        // from_checkpoint: include original input + completed assistant/tool
        // exchanges so the model resumes from where the run failed.
        for row in &rows {
            match row.kind.as_str() {
                runner::KIND_INPUT | runner::KIND_ENVIRONMENTS => {
                    let message = transcript_to_message(&row.payload)?;
                    input.push(runner::TurnMessage {
                        kind: if row.kind == runner::KIND_INPUT {
                            runner::KIND_INPUT
                        } else {
                            runner::KIND_ENVIRONMENTS
                        },
                        message,
                    });
                }
                runner::KIND_RUN_END | runner::KIND_RUN_ERROR | runner::KIND_RUN_INTERRUPTED => {
                    break;
                }
                "message" => {
                    let message = transcript_to_message(&row.payload)?;
                    input.push(runner::TurnMessage {
                        kind: runner::KIND_INPUT,
                        message,
                    });
                }
                _ => {}
            }
        }
    }

    if input.is_empty() {
        return Err(AppError::BadRequest(
            "could not reconstruct input from the failed run".into(),
        ));
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
        .map_err(|err| AppError::db(err, "runs.retry.insert_run"))?;

    let input_events = runner::record_input(&mut conn, run_id, &input).await?;
    drop(conn);

    let sender = runner::open_run(&state.runs, session_id);
    let interrupt = runner::open_interrupt(&state.interrupts, run_id);
    for event in input_events {
        let _ = sender.send(event);
    }

    runner::spawn_run(
        state,
        sender.clone(),
        runner::RunRequest {
            run_id,
            session_id,
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
        Json(RetryResponse {
            run_id,
            thread_id,
            mode,
        }),
    ))
}

fn transcript_to_message(payload: &Value) -> ApiResult<llm::Message> {
    let role_str = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let role = match role_str {
        "assistant" => llm::Role::Assistant,
        "system" => llm::Role::User,
        _ => llm::Role::User,
    };

    let content = match payload.get("content") {
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                let block_type = block.get("type").and_then(Value::as_str)?;
                match block_type {
                    "text" => {
                        let text = block.get("text").and_then(Value::as_str)?.to_owned();
                        Some(llm::ContentBlock::Text { text })
                    }
                    _ => None,
                }
            })
            .collect(),
        Some(Value::String(text)) => vec![llm::ContentBlock::Text {
            text: text.clone(),
        }],
        _ => vec![],
    };

    Ok(llm::Message { role, content })
}
