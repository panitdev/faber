//! Per-user CRUD for model presets.
//!
//! A preset is one provider serving one model, owned either by the caller or
//! by the system. A caller sees both halves and may change only their own.
//! System rows (`user_id IS NULL`) are the catalog's, fetched and upserted at
//! boot by [`crate::models::model_preset::replace_all`] and read-only here.
//!
//! A preset references a provider through `model_provider_id`; the API
//! requires that provider to be the caller's own, so the ownership of the two
//! always agrees. That invariant is what lets a system refresh update its
//! providers without touching anyone's presets. It may also link a creator
//! model through `creator_model_id` — any of them, since they are all the
//! system's — and then stores only what it says differently.
//!
//! Every read resolves: the response carries the preset as it reads, the
//! creator model's description under the preset's own overrides, and the
//! overrides themselves beside it so an editor can tell what is the preset's
//! and what is inherited.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, OptionalExtension, PgTextExpressionMethods, QueryDsl,
    SelectableHelper,
    dsl::sql,
    pg::Pg,
    sql_types::{Bool, Text},
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{
        creator_model::{CreatorModelRow, to_db},
        model_preset::{
            Joined, ModelPresetRow, NewModelPreset, UpdateModelPreset, resolve_joined,
        },
        model_provider::ModelProviderRow,
    },
    routes::{clamp_limit, deserialize_optional_field, escape_like},
    schema::{creator_models, model_presets, model_providers},
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

/// Absent fields do not filter. `vision`, `reasoning`, and `tool_call` given
/// as `false` require the *absence* of the capability, which is a different
/// question from not asking; each is read from the preset as it resolves, so
/// an inherited capability counts. `owned` splits the two halves a caller can
/// see: `true` for their own presets, `false` for the system's.
#[derive(Deserialize)]
struct ListQuery {
    /// A provider key, e.g. `anthropic`. Matches the caller's provider and the
    /// system's when both use the key; `model_provider_id` names one.
    provider: Option<String>,
    /// One provider row: the presets served by exactly that provider.
    model_provider_id: Option<Uuid>,
    /// A creator model id, e.g. `anthropic/claude-opus-5`: every provider
    /// serving that model.
    base_model: Option<String>,
    /// Case-insensitive substring of a model or provider id or name.
    q: Option<String>,
    vision: Option<bool>,
    reasoning: Option<bool>,
    tool_call: Option<bool>,
    owned: Option<bool>,
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
/// addresses; the flattened [`presets::Preset`] keeps its `id` as the model
/// id as served.
#[derive(Serialize)]
struct PresetResponse {
    preset_id: Uuid,
    /// Whether this preset belongs to the caller, as opposed to the system.
    owned: bool,
    created_at: DateTime<Utc>,
    /// The provider row this is served by; `provider` is its key.
    model_provider_id: Uuid,
    /// The linked creator model's row handle; `base_model` is its id.
    creator_model_id: Option<Uuid>,
    /// What this preset states itself. Every other descriptive field is the
    /// creator model's.
    overrides: presets::Overrides,
    #[serde(flatten)]
    preset: presets::Preset,
}

impl PresetResponse {
    fn new(joined: &Joined, owner: Uuid) -> Self {
        let row = &joined.0;
        Self {
            preset_id: row.id,
            owned: row.user_id == Some(owner),
            created_at: row.created_at,
            model_provider_id: row.model_provider_id,
            creator_model_id: row.creator_model_id,
            overrides: row.overrides(),
            preset: resolve_joined(joined),
        }
    }
}

/// A capability as the preset resolves it: its own override, or else its
/// creator model's, or else unstated.
fn resolved_flag(column: &'static str) -> String {
    format!("COALESCE(model_presets.{column}, creator_models.{column}, false)")
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
                .left_join(creator_models::table)
                .filter(
                    model_presets::user_id
                        .eq($owner)
                        .or(model_presets::user_id.is_null()),
                )
                .into_boxed::<Pg>();

            if let Some(provider) = $params.provider.as_deref() {
                query = query.filter(model_providers::provider_id.eq(provider));
            }

            if let Some(model_provider_id) = $params.model_provider_id {
                query = query.filter(model_presets::model_provider_id.eq(model_provider_id));
            }

            if let Some(base_model) = $params.base_model.as_deref() {
                query = query.filter(creator_models::model_id.eq(base_model));
            }

            if let Some(owned) = $params.owned {
                query = if owned {
                    query.filter(model_presets::user_id.eq($owner))
                } else {
                    query.filter(model_presets::user_id.is_null())
                };
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
                        .or(sql::<Bool>("COALESCE(model_presets.name, creator_models.name) ILIKE ")
                            .bind::<Text, _>(pattern.clone()))
                        .or(creator_models::model_id.ilike(pattern.clone()))
                        .or(model_providers::provider_id.ilike(pattern.clone()))
                        .or(model_providers::name.ilike(pattern)),
                );
            }

            if let Some(vision) = $params.vision {
                query = query.filter(
                    sql::<Bool>(
                        "COALESCE(jsonb_exists(COALESCE(model_presets.modalities, \
                         creator_models.modalities) -> 'input', 'image'), false) = ",
                    )
                    .bind::<Bool, _>(vision),
                );
            }
            for (column, wanted) in [
                ("reasoning", $params.reasoning),
                ("tool_call", $params.tool_call),
            ] {
                if let Some(wanted) = wanted {
                    query = query.filter(
                        sql::<Bool>(&format!("{} = ", resolved_flag(column)))
                            .bind::<Bool, _>(wanted),
                    );
                }
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

    let rows: Vec<Joined> = scoped!(&params, user.id)
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
            Option::<CreatorModelRow>::as_select(),
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
            .map(|joined| PresetResponse::new(joined, user.id))
            .collect(),
    }))
}

async fn get_one(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PresetResponse>> {
    let mut conn = state.db.get().await?;

    let joined = visible_preset(&mut conn, id, user.id)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(PresetResponse::new(&joined, user.id)))
}

#[derive(Deserialize)]
struct CreateRequest {
    /// The caller's own provider this preset is served by.
    provider_id: Uuid,
    /// The model id as served, e.g. `claude-opus-5`.
    id: String,
    /// The creator model this serves, when there is one. Its description is
    /// what `overrides` is laid over.
    creator_model_id: Option<Uuid>,
    /// What this preset states itself. With no creator model, everything it
    /// knows; with one, only what differs.
    #[serde(default)]
    overrides: presets::Overrides,
    cost: Option<presets::Cost>,
    reasoning_options: Option<Value>,
    status: Option<String>,
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
    let overrides = trimmed(input.overrides)?;

    let mut conn = state.db.get().await?;

    let provider = owned_provider(&mut conn, input.provider_id, user.id).await?;
    if let Some(creator_model_id) = input.creator_model_id {
        existing_creator_model(&mut conn, creator_model_id).await?;
    }

    let serving = presets::Serving {
        cost: input.cost,
        reasoning_options: input.reasoning_options,
        interleaved: None,
        status: input.status,
    };
    let new = NewModelPreset::new(
        Uuid::now_v7(),
        Some(user.id),
        provider.id,
        model_id.to_owned(),
        input.creator_model_id,
        &overrides,
        &serving,
    );

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

    let joined = visible_preset(&mut conn, inserted.id, user.id)
        .await?
        .ok_or(AppError::Internal)?;

    Ok((
        StatusCode::CREATED,
        Json(PresetResponse::new(&joined, user.id)),
    ))
}

#[derive(Deserialize)]
struct UpdateRequest {
    /// Move the preset to another of the caller's providers.
    provider_id: Option<Uuid>,
    id: Option<String>,
    /// `null` unlinks the creator model; the preset then states only its own
    /// overrides.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    creator_model_id: Option<Option<Uuid>>,
    /// Replaces every override at once. A field left out is cleared back to
    /// "as the creator model says", not kept — an editor sends the whole set.
    overrides: Option<presets::Overrides>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    cost: Option<Option<presets::Cost>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    reasoning_options: Option<Option<Value>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    status: Option<Option<String>>,
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

    let mut conn = state.db.get().await?;

    if let Some(provider_id) = input.provider_id {
        owned_provider(&mut conn, provider_id, user.id).await?;
    }
    if let Some(Some(creator_model_id)) = input.creator_model_id {
        existing_creator_model(&mut conn, creator_model_id).await?;
    }

    let mut patch = UpdateModelPreset {
        model_provider_id: input.provider_id,
        model_id: model_id.map(str::to_owned),
        creator_model_id: input.creator_model_id,
        cost: input.cost.map(|cost| cost.as_ref().map(to_db)),
        reasoning_options: input.reasoning_options,
        status: input.status,
        ..Default::default()
    };
    if let Some(overrides) = input.overrides {
        patch.set_overrides(&trimmed(overrides)?);
    }

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

    let joined = visible_preset(&mut conn, updated.id, user.id)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(PresetResponse::new(&joined, user.id)))
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

/// An override given as blank text means "not stated", the same as leaving it
/// out — a name of `"  "` would otherwise shadow the creator model's.
fn trimmed(mut overrides: presets::Overrides) -> ApiResult<presets::Overrides> {
    for field in [
        &mut overrides.name,
        &mut overrides.description,
        &mut overrides.family,
        &mut overrides.knowledge,
        &mut overrides.release_date,
        &mut overrides.last_updated,
    ] {
        *field = field
            .take()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
    }
    for date in [
        &overrides.knowledge,
        &overrides.release_date,
        &overrides.last_updated,
    ]
    .into_iter()
    .flatten()
    {
        if !is_catalog_date(date) {
            return Err(AppError::BadRequest(format!(
                "'{date}' is not a date; use YYYY-MM or YYYY-MM-DD"
            )));
        }
    }
    Ok(overrides)
}

/// `YYYY-MM` or `YYYY-MM-DD`, the forms the catalog writes dates in.
fn is_catalog_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    match bytes.len() {
        7 => digits(0..4) && bytes[4] == b'-' && digits(5..7),
        10 => digits(0..4) && bytes[4] == b'-' && digits(5..7) && bytes[7] == b'-' && digits(8..10),
        _ => false,
    }
}

/// Reads one visible preset, resolved against its provider and creator model.
async fn visible_preset(
    conn: &mut AsyncPgConnection,
    id: Uuid,
    owner: Uuid,
) -> ApiResult<Option<Joined>> {
    model_presets::table
        .inner_join(model_providers::table)
        .left_join(creator_models::table)
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
            Option::<CreatorModelRow>::as_select(),
        ))
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "model_presets.get"))
}

/// Reads one provider the caller owns, or fails the request.
async fn owned_provider(
    conn: &mut AsyncPgConnection,
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

/// Fails the request unless `id` names a creator model.
async fn existing_creator_model(conn: &mut AsyncPgConnection, id: Uuid) -> ApiResult<()> {
    creator_models::table
        .filter(creator_models::id.eq(id))
        .select(creator_models::id)
        .first::<Uuid>(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "model_presets.creator_model"))?
        .map(|_| ())
        .ok_or_else(|| AppError::BadRequest("creator model not found".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_dates_are_month_or_day_precision() {
        assert!(is_catalog_date("2026-03"));
        assert!(is_catalog_date("2026-03-01"));
        assert!(!is_catalog_date("1729036800"));
        assert!(!is_catalog_date("2026/03/01"));
        assert!(!is_catalog_date("2026-3-1"));
    }

    #[test]
    fn a_blank_override_is_not_stated() {
        let overrides = trimmed(presets::Overrides {
            name: Some("   ".into()),
            family: Some(" gpt ".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(overrides.name, None);
        assert_eq!(overrides.family.as_deref(), Some("gpt"));
    }

    #[test]
    fn a_malformed_date_is_refused() {
        assert!(
            trimmed(presets::Overrides {
                release_date: Some("last tuesday".into()),
                ..Default::default()
            })
            .is_err()
        );
    }
}
