//! A project's agent definitions: the kinds of agent its sessions may spawn.
//!
//! | Method | Path | |
//! |---|---|---|
//! | `GET` | `/api/projects/{id}/agents` | Every definition, the built-in `general-purpose` first |
//! | `POST` | `/api/projects/{id}/agents` | `{ name, description, prompt?, model?, plugins? }` |
//! | `PATCH` | `/api/projects/{id}/agents/{name}` | Any of those; `model` or `plugins` `null` is the parent's |
//! | `DELETE` | `/api/projects/{id}/agents/{name}` | Sessions already spawned as it carry on |
//!
//! Only the project's owner reaches these. `general-purpose` answers 409
//! `builtin` to a change. A `model` is one of the owner's aliases and each of
//! `plugins` is a plugin the project has bound, both checked when saved.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{deserialize_optional_field, projects::owned};
use crate::{
    auth::AuthUser,
    definitions::{self, Definition},
    error::ApiResult,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects/{id}/agents", get(list).post(create))
        .route(
            "/api/projects/{id}/agents/{name}",
            patch(update).delete(remove),
        )
}

#[derive(Serialize)]
struct DefinitionResponse {
    name: String,
    description: String,
    prompt: String,
    /// The owner's model alias its sessions run on, or `null` for the
    /// parent's.
    model: Option<String>,
    /// The project's plugins its sessions get, or `null` for the parent's.
    plugins: Option<Vec<String>>,
    /// `true` for `general-purpose`, which cannot be changed or deleted.
    builtin: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<DateTime<Utc>>,
}

fn response(definition: Definition) -> DefinitionResponse {
    DefinitionResponse {
        builtin: definition.builtin(),
        name: definition.name,
        description: definition.description,
        prompt: definition.prompt,
        model: definition.model_alias,
        plugins: definition.plugins,
        updated_at: definition.updated_at,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    name: String,
    description: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    plugins: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRequest {
    name: Option<String>,
    description: Option<String>,
    prompt: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    model: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    plugins: Option<Option<Vec<String>>>,
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<DefinitionResponse>>> {
    let mut conn = state.db.get().await?;
    owned(&mut conn, user.id, id).await?;
    let all = definitions::list(&mut conn, id).await?;
    Ok(Json(all.into_iter().map(response).collect()))
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<DefinitionResponse>)> {
    let mut conn = state.db.get().await?;
    owned(&mut conn, user.id, id).await?;
    let new = definitions::New {
        name: input.name,
        description: input.description,
        prompt: input.prompt,
        model: input.model,
        plugins: input.plugins,
    };
    let made = definitions::create(&mut conn, user.id, id, new).await?;
    Ok((StatusCode::CREATED, Json(response(made))))
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, name)): Path<(Uuid, String)>,
    Json(input): Json<UpdateRequest>,
) -> ApiResult<Json<DefinitionResponse>> {
    let mut conn = state.db.get().await?;
    owned(&mut conn, user.id, id).await?;
    let patch = definitions::Patch {
        name: input.name,
        description: input.description,
        prompt: input.prompt,
        model: input.model,
        plugins: input.plugins,
    };
    let changed = definitions::update(&mut conn, user.id, id, &name, patch).await?;
    Ok(Json(response(changed)))
}

async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, name)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    owned(&mut conn, user.id, id).await?;
    definitions::delete(&mut conn, id, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}
