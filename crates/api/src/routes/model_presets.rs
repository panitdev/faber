//! Per-user CRUD for model presets.
//!
//! A preset is a third party's description of a model, owned either by the
//! caller or by the system. A caller sees both halves and may change only
//! their own. System rows (`user_id IS NULL`) are the directory's, fetched and
//! reseeded at boot by [`crate::models::model_preset::replace_all`] and
//! read-only here.
//!
//! A preset references a provider through `model_provider_id`; the API
//! requires that provider to be the caller's own, so the ownership of the two
//! always agrees. That invariant is what lets a system refresh replace its
//! providers without deleting anyone's presets.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, OptionalExtension, PgTextExpressionMethods, QueryDsl,
    SelectableHelper, pg::Pg,
};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{
        model_preset::{
            ModelPresetRow, NewModelPreset, UpdateModelPreset, limit_to_db, modalities_to_db,
        },
        model_provider::ModelProviderRow,
    },
    routes::{clamp_limit, deserialize_optional_field},
    schema::{model_presets, model_providers},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/model-presets", get(list).post(create))
        .route(
            "/api/model-presets/{id}",
            get(get_one).patch(update).delete(remove),
        )
}

/// Absent fields do not filter. `vision`, `reasoning`, and `tools` given as
/// `false` require the *absence* of the capability, which is a different
/// question from not asking.
#[derive(Deserialize)]
struct ListQuery {
    /// A provider key, e.g. `anthropic`.
    provider: Option<String>,
    /// Case-insensitive substring of a model or provider id or name.
    q: Option<String>,
    vision: Option<bool>,
    reasoning: Option<bool>,
    tools: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// A page of the catalog. `total` is the count *after* filtering, so a client
/// can page without guessing how many rows a filter left.
#[derive(Serialize)]
struct PageResponse {
    total: usize,
    limit: usize,
    offset: usize,
    items: Vec<PresetResponse>,
}

/// A preset as the client sees it. `preset_id` is the row handle CRUD
/// addresses; the flattened [`presets::Preset`] keeps its `id` as the model id.
#[derive(Serialize)]
struct PresetResponse {
    preset_id: Uuid,
    /// Whether this preset belongs to the caller, as opposed to the system.
    owned: bool,
    created_at: DateTime<Utc>,
    #[serde(flatten)]
    preset: presets::Preset,
}

impl PresetResponse {
    fn new(row: &ModelPresetRow, provider: &str, provider_name: &str, owner: Uuid) -> Self {
        Self {
            preset_id: row.id,
            owned: row.user_id == Some(owner),
            created_at: row.created_at,
            preset: row.to_preset(provider, provider_name),
        }
    }
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(params): Query<ListQuery>,
) -> ApiResult<Json<PageResponse>> {
    let mut conn = state.db.get().await?;

    // The same query feeds both the count and the page, so it is built once
    // per call. A macro rather than a closure because the boxed statement
    // borrows the query parameters and has no nameable return type.
    macro_rules! scoped {
        ($params:expr, $owner:expr) => {{
            let mut query = model_presets::table
                .inner_join(model_providers::table)
                .filter(
                    model_presets::user_id
                        .eq($owner)
                        .or(model_presets::user_id.is_null()),
                )
                .into_boxed::<Pg>();

            if let Some(provider) = $params.provider.as_deref() {
                query = query.filter(model_providers::provider_id.eq(provider));
            }

            if let Some(needle) = $params
                .q
                .as_deref()
                .map(str::trim)
                .filter(|needle| !needle.is_empty())
            {
                let pattern = format!("%{}%", escape_like(needle));
                query = query.filter(
                    model_presets::model_id
                        .ilike(pattern.clone())
                        .or(model_presets::name.ilike(pattern.clone()))
                        .or(model_providers::provider_id.ilike(pattern.clone()))
                        .or(model_providers::name.ilike(pattern)),
                );
            }

            if let Some(vision) = $params.vision {
                query = query.filter(model_presets::vision.eq(vision));
            }
            if let Some(reasoning) = $params.reasoning {
                query = query.filter(model_presets::reasoning.eq(reasoning));
            }
            if let Some(tools) = $params.tools {
                query = query.filter(model_presets::tools.eq(tools));
            }

            query
        }};
    }

    let total: i64 = scoped!(&params, user.id)
        .count()
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "model_presets.list_count"))?;

    let limit = clamp_limit(params.limit);
    let offset = params.offset.unwrap_or(0).max(0);

    let rows: Vec<(ModelPresetRow, String, String)> = scoped!(&params, user.id)
        .order_by((
            model_providers::provider_id.asc(),
            model_presets::model_id.asc(),
        ))
        .limit(limit)
        .offset(offset)
        .select((
            ModelPresetRow::as_select(),
            model_providers::provider_id,
            model_providers::name,
        ))
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "model_presets.list"))?;

    Ok(Json(PageResponse {
        total: total as usize,
        limit: limit as usize,
        offset: offset as usize,
        items: rows
            .iter()
            .map(|(row, provider, provider_name)| {
                PresetResponse::new(row, provider, provider_name, user.id)
            })
            .collect(),
    }))
}

async fn get_one(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PresetResponse>> {
    let mut conn = state.db.get().await?;

    let (row, provider, provider_name) = preset_row_query(&mut conn, id, user.id)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(PresetResponse::new(
        &row,
        &provider,
        &provider_name,
        user.id,
    )))
}

#[derive(Deserialize)]
struct CreateRequest {
    /// The caller's own provider this preset is published by.
    provider_id: Uuid,
    /// The model id as served, e.g. `claude-opus-5`.
    id: String,
    name: String,
    #[serde(default)]
    capabilities: presets::Capabilities,
    #[serde(default)]
    pricing: presets::Pricing,
    #[serde(default)]
    limits: presets::Limits,
    #[serde(default)]
    modalities: presets::Modalities,
    release_date: Option<i64>,
    last_updated: Option<i64>,
    knowledge_cutoff: Option<i64>,
    open_weights: Option<bool>,
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<PresetResponse>)> {
    let model_id = input.id.trim();
    if model_id.is_empty() {
        return Err(AppError::BadRequest("id is required".into()));
    }
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }

    let mut conn = state.db.get().await?;

    let provider = owned_provider(&mut conn, input.provider_id, user.id).await?;

    let preset = presets::Preset {
        provider: provider.provider_id.clone(),
        provider_name: provider.name.clone(),
        id: model_id.to_owned(),
        name: name.to_owned(),
        capabilities: input.capabilities,
        pricing: input.pricing,
        limits: input.limits,
        modalities: input.modalities,
        release_date: input.release_date,
        last_updated: input.last_updated,
        knowledge_cutoff: input.knowledge_cutoff,
        open_weights: input.open_weights,
    };

    let new = NewModelPreset::owned(Uuid::now_v7(), user.id, provider.id, &preset);

    let inserted: ModelPresetRow = diesel::insert_into(model_presets::table)
        .values(&new)
        .returning(ModelPresetRow::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("a preset for '{model_id}' already exists")),
            other => AppError::db(other, "model_presets.create"),
        })?;

    Ok((
        StatusCode::CREATED,
        Json(PresetResponse::new(
            &inserted,
            &provider.provider_id,
            &provider.name,
            user.id,
        )),
    ))
}

#[derive(Deserialize)]
struct UpdateRequest {
    /// Move the preset to another of the caller's providers.
    provider_id: Option<Uuid>,
    id: Option<String>,
    name: Option<String>,
    capabilities: Option<presets::Capabilities>,
    pricing: Option<presets::Pricing>,
    limits: Option<presets::Limits>,
    modalities: Option<presets::Modalities>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    release_date: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    last_updated: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    knowledge_cutoff: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    open_weights: Option<Option<bool>>,
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRequest>,
) -> ApiResult<Json<PresetResponse>> {
    let model_id = input.id.as_deref().map(str::trim);
    if let Some(model_id) = model_id
        && model_id.is_empty()
    {
        return Err(AppError::BadRequest("id cannot be empty".into()));
    }
    let name = input.name.as_deref().map(str::trim);
    if let Some(name) = name
        && name.is_empty()
    {
        return Err(AppError::BadRequest("name cannot be empty".into()));
    }

    let mut conn = state.db.get().await?;

    if let Some(provider_id) = input.provider_id {
        owned_provider(&mut conn, provider_id, user.id).await?;
    }

    let (vision, attachment, reasoning, tools, structured_output, temperature) =
        match input.capabilities {
            Some(capabilities) => (
                Some(capabilities.vision),
                Some(capabilities.attachment),
                Some(capabilities.reasoning),
                Some(capabilities.tools),
                Some(capabilities.structured_output),
                Some(capabilities.temperature),
            ),
            None => (None, None, None, None, None, None),
        };

    let (
        price_input,
        price_output,
        price_cache_read,
        price_cache_write,
        price_input_audio,
        price_output_audio,
        price_reasoning,
    ) = match input.pricing {
        Some(pricing) => (
            Some(pricing.input),
            Some(pricing.output),
            Some(pricing.cache_read),
            Some(pricing.cache_write),
            Some(pricing.input_audio),
            Some(pricing.output_audio),
            Some(pricing.reasoning),
        ),
        None => (None, None, None, None, None, None, None),
    };

    let (limit_context, limit_input, limit_output) = match input.limits {
        Some(limits) => (
            Some(limit_to_db(limits.context)),
            Some(limit_to_db(limits.input)),
            Some(limit_to_db(limits.output)),
        ),
        None => (None, None, None),
    };

    let (modalities_input, modalities_output) = match input.modalities {
        Some(modalities) => (
            Some(modalities_to_db(&modalities.input)),
            Some(modalities_to_db(&modalities.output)),
        ),
        None => (None, None),
    };

    let patch = UpdateModelPreset {
        model_provider_id: input.provider_id,
        model_id: model_id.map(str::to_owned),
        name: name.map(str::to_owned),
        vision,
        attachment,
        reasoning,
        tools,
        structured_output,
        temperature,
        price_input,
        price_output,
        price_cache_read,
        price_cache_write,
        price_input_audio,
        price_output_audio,
        price_reasoning,
        limit_context,
        limit_input,
        limit_output,
        modalities_input,
        modalities_output,
        release_date: input.release_date,
        last_updated: input.last_updated,
        knowledge_cutoff: input.knowledge_cutoff,
        open_weights: input.open_weights,
    };

    let updated: ModelPresetRow = diesel::update(
        model_presets::table
            .filter(model_presets::id.eq(id))
            .filter(model_presets::user_id.eq(user.id)),
    )
    .set(patch)
    .returning(ModelPresetRow::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|err| match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => AppError::BadRequest("a preset with that model id already exists".into()),
        other => AppError::db(other, "model_presets.update"),
    })?;

    let provider = owned_provider(&mut conn, updated.model_provider_id, user.id).await?;

    Ok(Json(PresetResponse::new(
        &updated,
        &provider.provider_id,
        &provider.name,
        user.id,
    )))
}

async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        model_presets::table
            .filter(model_presets::id.eq(id))
            .filter(model_presets::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "model_presets.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Reads one visible preset with its provider fields.
async fn preset_row_query(
    conn: &mut diesel_async::AsyncPgConnection,
    id: Uuid,
    owner: Uuid,
) -> ApiResult<Option<(ModelPresetRow, String, String)>> {
    model_presets::table
        .inner_join(model_providers::table)
        .filter(model_presets::id.eq(id))
        .filter(
            model_presets::user_id
                .eq(owner)
                .or(model_presets::user_id.is_null()),
        )
        .select((
            ModelPresetRow::as_select(),
            model_providers::provider_id,
            model_providers::name,
        ))
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "model_presets.get"))
}

/// Reads one provider the caller owns, or fails the request.
async fn owned_provider(
    conn: &mut diesel_async::AsyncPgConnection,
    id: Uuid,
    owner: Uuid,
) -> ApiResult<ModelProviderRow> {
    model_providers::table
        .filter(model_providers::id.eq(id))
        .filter(model_providers::user_id.eq(owner))
        .select(ModelProviderRow::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "model_presets.owned_provider"))?
        .ok_or_else(|| AppError::BadRequest("provider not found".into()))
}

/// Escapes `LIKE` metacharacters so a search is a literal substring, not a
/// pattern. PostgreSQL's default escape character is backslash.
fn escape_like(needle: &str) -> String {
    let mut escaped = String::with_capacity(needle.len());
    for character in needle.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape_like;

    #[test]
    fn like_metacharacters_are_escaped() {
        assert_eq!(escape_like("100%_raw"), "100\\%\\_raw");
        assert_eq!(escape_like("back\\slash"), "back\\\\slash");
        assert_eq!(escape_like("plain"), "plain");
    }
}
