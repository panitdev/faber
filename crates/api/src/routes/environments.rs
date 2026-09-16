//! What a session can be told to reach, and what it currently reaches.
//!
//! Two lists, deliberately different. `GET /api/environments` is what the
//! caller *could* tag — the source the client's `@` picker reads, so a name it
//! offers is a name that resolves. `GET /api/sessions/{id}/environments` is
//! what a session has actually been given, which is a subset and grows only
//! when a user tags one in a message.
//!
//! There is no route here that the model can reach. Binding is the user's:
//! enumeration reaches bound labels, never the machine, and nothing an agent
//! calls appears in this file.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get},
};
use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::Serialize;
use uuid::Uuid;

use crate::{
    access::authorize_session,
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{now_epoch, session::SessionEnvironment},
    schema::session_environment,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/environments", get(list_candidates))
        .route("/api/sessions/{id}/environments", get(list_bound))
        .route("/api/sessions/{id}/environments/{label}", delete(unbind))
}

/// One name the caller could tag, and enough about it to choose between two
/// that look alike.
#[derive(Serialize)]
struct CandidateResponse {
    /// What the user types after `@`. Short where it can be, qualified as
    /// `host/name` where two would otherwise collide.
    label: String,
    kind: &'static str,
    host_id: Uuid,
    host_name: String,
    container_id: Option<Uuid>,
    root_path: String,
    /// Operator intent on the host. Shown rather than filtered: a name that is
    /// missing from the picker looks like a name that does not exist.
    disabled: bool,
    /// When true, new sessions auto-bind this environment without an @mention.
    bind_by_default: bool,
}

#[derive(Serialize)]
struct BoundResponse {
    label: String,
    host_id: Uuid,
    container_id: Option<Uuid>,
    added_at: i64,
    /// Set once the label has been unbound. The row stays, and so does the
    /// claim on the name — a label that meant two machines would make the
    /// earlier half of the transcript wrong.
    removed_at: Option<i64>,
}

fn bound_response(row: &SessionEnvironment) -> BoundResponse {
    BoundResponse {
        label: row.label.clone(),
        host_id: row.host_id,
        container_id: row.container_id,
        added_at: row.added_at,
        removed_at: row.removed_at,
    }
}

async fn list_candidates(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<CandidateResponse>>> {
    let mut conn = state.db.get().await?;
    let found = crate::environments::candidates(&mut conn, user.id).await?;

    Ok(Json(
        found
            .into_iter()
            .map(|candidate| CandidateResponse {
                label: candidate.label,
                kind: candidate.kind,
                host_id: candidate.host_id,
                host_name: candidate.host_name,
                container_id: candidate.container_id,
                root_path: candidate.root_path,
                disabled: candidate.disabled,
                bind_by_default: candidate.bind_by_default,
            })
            .collect(),
    ))
}

/// Every binding this session has ever had, tombstones included — that is the
/// log, and the log is what replays.
async fn list_bound(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<BoundResponse>>> {
    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    let rows: Vec<SessionEnvironment> = session_environment::table
        .filter(session_environment::session_id.eq(id))
        .order(session_environment::added_at.asc())
        .select(SessionEnvironment::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "environments.list_bound"))?;

    Ok(Json(rows.iter().map(bound_response).collect()))
}

/// Tombstones a binding. Nothing is removed.
///
/// The label stays claimed afterwards and cannot be tagged onto a different
/// machine, which is the whole reason this is not a delete: calls recorded as
/// `label:path` earlier in the conversation have to keep meaning what they
/// meant. A later call against the label answers "not bound".
async fn unbind(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, label)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    authorize_session(&mut conn, user.id, id).await?;

    let updated = diesel::update(
        session_environment::table
            .filter(session_environment::session_id.eq(id))
            .filter(session_environment::label.eq(&label))
            .filter(session_environment::removed_at.is_null()),
    )
    .set(session_environment::removed_at.eq(Some(now_epoch())))
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "environments.unbind"))?;

    if updated == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}
