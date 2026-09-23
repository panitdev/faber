//! Per-user CRUD for model providers.
//!
//! A provider is metadata a model preset points at. A caller sees their own
//! providers and the system's, and may change only their own. System rows
//! (`user_id IS NULL`) are the directory's, reseeded at boot and read-only
//! here.

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper,
};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::model_provider::{ModelProviderRow, NewModelProvider, UpdateModelProvider},
    routes::deserialize_optional_field,
    schema::{model_presets, model_providers},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/model-providers", get(list).post(create))
        .route(
            "/api/model-providers/{id}",
            get(get_one).patch(update).delete(remove),
        )
}

/// A provider as the client sees it. `provider_id` is the row handle CRUD
/// addresses; `id` is the publisher's key, e.g. `anthropic`.
#[derive(Serialize)]
struct ProviderResponse {
    provider_id: Uuid,
    id: String,
    name: String,
    website: Option<String>,
    api_base_url: Option<String>,
    model_count: usize,
    /// Whether this provider belongs to the caller, as opposed to the system.
    owned: bool,
    created_at: DateTime<Utc>,
}

fn provider_response(row: &ModelProviderRow, owner: Uuid, model_count: usize) -> ProviderResponse {
    ProviderResponse {
        provider_id: row.id,
        id: row.provider_id.clone(),
        name: row.name.clone(),
        website: row.website.clone(),
        api_base_url: row.api_base_url.clone(),
        model_count,
        owned: row.user_id == Some(owner),
        created_at: row.created_at,
    }
}

/// The caller's own providers plus the system's, with how many visible presets
/// each one publishes.
async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<ProviderResponse>>> {
    let mut conn = state.db.get().await?;

    let rows: Vec<ModelProviderRow> = model_providers::table
        .filter(
            model_providers::user_id
                .eq(user.id)
                .or(model_providers::user_id.is_null()),
        )
        .order_by(model_providers::provider_id.asc())
        .select(ModelProviderRow::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "model_providers.list"))?;

    let counts: Vec<(Uuid, i64)> = model_presets::table
        .filter(
            model_presets::user_id
                .eq(user.id)
                .or(model_presets::user_id.is_null()),
        )
        .group_by(model_presets::model_provider_id)
        .select((model_presets::model_provider_id, diesel::dsl::count_star()))
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "model_providers.list_counts"))?;
    let counts: HashMap<Uuid, usize> = counts
        .into_iter()
        .map(|(id, count)| (id, count as usize))
        .collect();

    Ok(Json(
        rows.iter()
            .map(|row| {
                let model_count = counts.get(&row.id).copied().unwrap_or(0);
                provider_response(row, user.id, model_count)
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct CreateRequest {
    /// The publisher's key, e.g. `anthropic`.
    id: String,
    name: String,
    website: Option<String>,
    api_base_url: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<ProviderResponse>)> {
    let key = input.id.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest("id is required".into()));
    }
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }

    let mut conn = state.db.get().await?;

    let new = NewModelProvider {
        id: Uuid::now_v7(),
        user_id: Some(user.id),
        provider_id: key.to_owned(),
        name: name.to_owned(),
        website: input.website,
        api_base_url: input.api_base_url,
    };

    let inserted: ModelProviderRow = diesel::insert_into(model_providers::table)
        .values(&new)
        .returning(ModelProviderRow::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("a provider named '{key}' already exists")),
            other => AppError::db(other, "model_providers.create"),
        })?;

    Ok((
        StatusCode::CREATED,
        Json(provider_response(&inserted, user.id, 0)),
    ))
}

async fn get_one(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ProviderResponse>> {
    let mut conn = state.db.get().await?;

    let row: ModelProviderRow = model_providers::table
        .filter(model_providers::id.eq(id))
        .filter(
            model_providers::user_id
                .eq(user.id)
                .or(model_providers::user_id.is_null()),
        )
        .select(ModelProviderRow::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "model_providers.get"))?
        .ok_or(AppError::NotFound)?;

    let model_count: i64 = model_presets::table
        .filter(model_presets::model_provider_id.eq(id))
        .filter(
            model_presets::user_id
                .eq(user.id)
                .or(model_presets::user_id.is_null()),
        )
        .count()
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "model_providers.get_count"))?;

    Ok(Json(provider_response(&row, user.id, model_count as usize)))
}

#[derive(Deserialize)]
struct UpdateRequest {
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    website: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    api_base_url: Option<Option<String>>,
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRequest>,
) -> ApiResult<Json<ProviderResponse>> {
    let name = input.name.as_deref().map(str::trim);
    if let Some(name) = name
        && name.is_empty()
    {
        return Err(AppError::BadRequest("name cannot be empty".into()));
    }

    let patch = UpdateModelProvider {
        name: name.map(str::to_owned),
        website: input.website,
        api_base_url: input.api_base_url,
    };

    let mut conn = state.db.get().await?;

    let updated: ModelProviderRow = diesel::update(
        model_providers::table
            .filter(model_providers::id.eq(id))
            .filter(model_providers::user_id.eq(user.id)),
    )
    .set(patch)
    .returning(ModelProviderRow::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|err| match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        other => AppError::db(other, "model_providers.update"),
    })?;

    let model_count: i64 = model_presets::table
        .filter(model_presets::model_provider_id.eq(id))
        .filter(
            model_presets::user_id
                .eq(user.id)
                .or(model_presets::user_id.is_null()),
        )
        .count()
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "model_providers.update_count"))?;

    Ok(Json(provider_response(
        &updated,
        user.id,
        model_count as usize,
    )))
}

/// Deletes the caller's provider and, by cascade, every preset under it. There
/// is no undo; the provider cannot be recovered, only re-created.
async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        model_providers::table
            .filter(model_providers::id.eq(id))
            .filter(model_providers::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "model_providers.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}
