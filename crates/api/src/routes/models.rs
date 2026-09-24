use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{patch, post},
};
use chrono::{DateTime, Utc};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper,
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::model_config::{
        ADVANCED_KEY, ModelConfig, NewModelConfig, REASONING_HISTORY_KEY, THINKING_KEY,
        UpdateModelConfig, Wire, parse_advanced_options, parse_reasoning_history,
    },
    models::model_preset::ModelPresetRow,
    models::thinking::parse_thinking_capability,
    routes::deserialize_optional_field,
    schema::{credentials, model_presets, model_providers, models},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/models", post(create).get(list))
        .route("/api/models/{id}", patch(update).delete(remove))
}

#[derive(Deserialize)]
struct CreateRequest {
    alias: String,
    base_url: String,
    wire: Wire,
    wire_id: String,
    family: Option<String>,
    credential_id: Option<Uuid>,
    #[serde(default)]
    params: Value,
    /// The preset that describes this model, when the caller knows one. `null`
    /// or absent leaves the row on the built-in empty preset.
    preset_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct UpdateRequest {
    alias: Option<String>,
    base_url: Option<String>,
    wire: Option<Wire>,
    wire_id: Option<String>,
    /// `null` explicitly clears the field; omitting it leaves it unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    family: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    credential_id: Option<Option<Uuid>>,
    params: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    preset_id: Option<Option<Uuid>>,
}

/// A model as the client sees it. `preset` is resolved here rather than left
/// to the client: a row with no `preset_id` is described by the built-in empty
/// preset, so the shape is the same whether or not the caller linked one.
#[derive(Serialize)]
struct ModelResponse {
    id: Uuid,
    alias: String,
    base_url: String,
    wire: String,
    wire_id: String,
    family: Option<String>,
    credential_id: Option<Uuid>,
    params: Value,
    preset_id: Option<Uuid>,
    preset: presets::Preset,
    created_at: DateTime<Utc>,
}

fn model_response(m: &ModelConfig, preset: presets::Preset) -> ModelResponse {
    ModelResponse {
        id: m.id,
        alias: m.alias.clone(),
        base_url: m.base_url.clone(),
        wire: m.wire.clone(),
        wire_id: m.wire_id.clone(),
        family: m.family.clone(),
        credential_id: m.credential_id,
        params: m.params.clone(),
        preset_id: m.preset_id,
        preset,
        created_at: m.created_at,
    }
}

/// Rejects a `params` blob whose reasoning-history setting, thinking knob, or
/// advanced options are not ones this service understands.
///
/// Checked here rather than at run time: a typo that quietly means "the wire
/// default" is a setting the user believes they made and cannot see fail.
fn validate_params(params: &Value) -> Result<(), AppError> {
    if let Some(value) = params.get(REASONING_HISTORY_KEY) {
        parse_reasoning_history(value).map_err(AppError::BadRequest)?;
    }
    // The thinking knob is what a session's selection is read against: a
    // level named here that the picker then offers has to be a level the run
    // can actually ask for.
    if let Some(value) = params.get(THINKING_KEY) {
        parse_thinking_capability(value).map_err(AppError::BadRequest)?;
    }
    if let Some(value) = params.get(ADVANCED_KEY) {
        parse_advanced_options(value).map_err(AppError::BadRequest)?;
    }
    Ok(())
}

fn validate_alias(alias: &str) -> Result<(), AppError> {
    if alias.is_empty() {
        return Err(AppError::BadRequest("alias is required".into()));
    }
    if alias.chars().count() > 100 {
        return Err(AppError::BadRequest(
            "alias must be 100 characters or fewer".into(),
        ));
    }
    Ok(())
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<ModelResponse>)> {
    let alias = input.alias.trim();
    validate_alias(alias)?;

    if input.base_url.trim().is_empty() {
        return Err(AppError::BadRequest("base_url is required".into()));
    }
    if input.wire_id.trim().is_empty() {
        return Err(AppError::BadRequest("wire_id is required".into()));
    }

    validate_params(&input.params)?;

    let wire_str = input.wire.as_str();

    let mut conn = state.db.get().await?;

    if let Some(cred_id) = input.credential_id {
        verify_credential(&mut conn, cred_id, user.id).await?;
    }

    if let Some(preset_id) = input.preset_id {
        visible_preset(&mut conn, preset_id, user.id)
            .await?
            .ok_or_else(|| AppError::BadRequest("preset not found".into()))?;
    }

    let new_model = NewModelConfig {
        id: Uuid::now_v7(),
        user_id: user.id,
        alias,
        base_url: input.base_url.trim(),
        wire: wire_str,
        wire_id: input.wire_id.trim(),
        family: input.family.as_deref(),
        credential_id: input.credential_id,
        params: input.params,
        preset_id: input.preset_id,
    };

    let inserted: ModelConfig = diesel::insert_into(models::table)
        .values(&new_model)
        .returning(ModelConfig::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("a model named '{alias}' already exists")),
            other => AppError::db(other, "models.create"),
        })?;

    let preset = display_preset(&mut conn, inserted.preset_id, user.id).await?;

    Ok((StatusCode::CREATED, Json(model_response(&inserted, preset))))
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<ModelResponse>>> {
    let mut conn = state.db.get().await?;

    let rows: Vec<ModelConfig> = models::table
        .filter(models::user_id.eq(user.id))
        .order_by(models::created_at.asc())
        .select(ModelConfig::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "models.list"))?;

    // Resolved in a second query rather than a join: a model without a preset
    // has nothing to join to, and `Option`-selecting a twenty-seven-column row
    // through a left join is more machinery than a lookup map.
    let preset_ids: Vec<Uuid> = rows.iter().filter_map(|m| m.preset_id).collect();
    let preset_rows: Vec<(Uuid, ModelPresetRow, String, String)> = model_presets::table
        .inner_join(model_providers::table)
        .filter(model_presets::id.eq_any(&preset_ids))
        .select((
            model_presets::id,
            ModelPresetRow::as_select(),
            model_providers::provider_id,
            model_providers::name,
        ))
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "models.list_presets"))?;

    let by_id: HashMap<Uuid, presets::Preset> = preset_rows
        .iter()
        .map(|(id, row, provider, provider_name)| (*id, row.to_preset(provider, provider_name)))
        .collect();

    Ok(Json(
        rows.iter()
            .map(|m| {
                let preset = m
                    .preset_id
                    .and_then(|id| by_id.get(&id).cloned())
                    .unwrap_or_default();
                model_response(m, preset)
            })
            .collect(),
    ))
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRequest>,
) -> ApiResult<Json<ModelResponse>> {
    if let Some(alias) = input.alias.as_deref() {
        validate_alias(alias.trim())?;
    }

    if let Some(ref base_url) = input.base_url {
        if base_url.trim().is_empty() {
            return Err(AppError::BadRequest("base_url cannot be empty".into()));
        }
    }

    if let Some(ref wire_id) = input.wire_id {
        if wire_id.trim().is_empty() {
            return Err(AppError::BadRequest("wire_id cannot be empty".into()));
        }
    }

    if let Some(params) = &input.params {
        validate_params(params)?;
    }

    let mut conn = state.db.get().await?;

    if let Some(Some(cred_id)) = input.credential_id {
        verify_credential(&mut conn, cred_id, user.id).await?;
    }

    if let Some(Some(preset_id)) = input.preset_id {
        visible_preset(&mut conn, preset_id, user.id)
            .await?
            .ok_or_else(|| AppError::BadRequest("preset not found".into()))?;
    }

    let alias_trimmed = input.alias.as_deref().map(str::trim);
    let base_url_trimmed = input.base_url.as_deref().map(str::trim);
    let wire_str = input.wire.as_ref().map(Wire::as_str);
    let wire_id_trimmed = input.wire_id.as_deref().map(str::trim);

    let patch = UpdateModelConfig {
        alias: alias_trimmed,
        base_url: base_url_trimmed,
        wire: wire_str,
        wire_id: wire_id_trimmed,
        family: input.family.as_ref().map(|opt| opt.as_deref()),
        credential_id: input.credential_id,
        params: input.params,
        preset_id: input.preset_id,
    };

    let updated: ModelConfig = diesel::update(
        models::table
            .filter(models::id.eq(id))
            .filter(models::user_id.eq(user.id)),
    )
    .set(patch)
    .returning(ModelConfig::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|err| match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => AppError::BadRequest("a model with that alias already exists".into()),
        other => AppError::db(other, "models.update"),
    })?;

    let preset = display_preset(&mut conn, updated.preset_id, user.id).await?;

    Ok(Json(model_response(&updated, preset)))
}

async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        models::table
            .filter(models::id.eq(id))
            .filter(models::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "models.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Fails unless `cred_id` names an API-key credential the caller owns.
async fn verify_credential(
    conn: &mut AsyncPgConnection,
    cred_id: Uuid,
    owner: Uuid,
) -> ApiResult<()> {
    let exists: Option<Uuid> = credentials::table
        .filter(credentials::id.eq(cred_id))
        .filter(credentials::user_id.eq(owner))
        .filter(credentials::kind.eq("api_key"))
        .select(credentials::id)
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "models.verify_credential"))?;
    if exists.is_none() {
        return Err(AppError::BadRequest("API key credential not found".into()));
    }
    Ok(())
}

/// Reads one preset the caller may reference: their own, or the system's.
async fn visible_preset(
    conn: &mut AsyncPgConnection,
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
        .map_err(|err| AppError::db(err, "models.visible_preset"))
}

/// The preset a model is described by, as the response carries it. A `None`
/// id, or a row that vanished between the write and this read, is the built-in
/// empty preset — a description that says nothing rather than a missing one.
async fn display_preset(
    conn: &mut AsyncPgConnection,
    preset_id: Option<Uuid>,
    owner: Uuid,
) -> ApiResult<presets::Preset> {
    match preset_id {
        None => Ok(presets::Preset::default()),
        Some(id) => Ok(match visible_preset(conn, id, owner).await? {
            Some((row, provider, provider_name)) => row.to_preset(&provider, &provider_name),
            None => presets::Preset::default(),
        }),
    }
}
