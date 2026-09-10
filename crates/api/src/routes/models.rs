use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{patch, post},
};
use chrono::{DateTime, Utc};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{
        model_config::{
            ADVANCED_KEY, ModelConfig, NewModelConfig, REASONING_HISTORY_KEY, THINKING_KEY,
            UpdateModelConfig, Wire, parse_advanced_options, parse_reasoning_history,
        },
        thinking::parse_thinking_capability,
    },
    routes::deserialize_optional_field,
    schema::{credentials, models},
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
    #[serde(default)]
    capabilities: Value,
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
    capabilities: Option<Value>,
}

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
    capabilities: Value,
    created_at: DateTime<Utc>,
}

fn model_response(m: &ModelConfig) -> ModelResponse {
    ModelResponse {
        id: m.id,
        alias: m.alias.clone(),
        base_url: m.base_url.clone(),
        wire: m.wire.clone(),
        wire_id: m.wire_id.clone(),
        family: m.family.clone(),
        credential_id: m.credential_id,
        params: m.params.clone(),
        capabilities: m.capabilities.clone(),
        created_at: m.created_at,
    }
}

/// Rejects a `capabilities` blob whose reasoning-history setting or thinking
/// knob is not one this service understands.
///
/// Checked here rather than at run time: a typo that quietly means "the wire
/// default" is a setting the user believes they made and cannot see fail.
fn validate_capabilities(capabilities: &Value) -> Result<(), AppError> {
    if let Some(value) = capabilities.get(REASONING_HISTORY_KEY) {
        parse_reasoning_history(value).map_err(AppError::BadRequest)?;
    }
    // The thinking knob is what a session's selection is read against: a
    // level named here that the picker then offers has to be a level the run
    // can actually ask for.
    if let Some(value) = capabilities.get(THINKING_KEY) {
        parse_thinking_capability(value).map_err(AppError::BadRequest)?;
    }
    Ok(())
}

/// Rejects a `params.advanced` blob the run path could not make sense of —
/// same reasoning as [`validate_capabilities`].
fn validate_params(params: &Value) -> Result<(), AppError> {
    let Some(value) = params.get(ADVANCED_KEY) else {
        return Ok(());
    };
    parse_advanced_options(value).map_err(AppError::BadRequest)?;
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

    validate_capabilities(&input.capabilities)?;
    validate_params(&input.params)?;

    let wire_str = input.wire.as_str();

    let mut conn = state.db.get().await?;

    if let Some(cred_id) = input.credential_id {
        let exists: Option<Uuid> = credentials::table
            .filter(credentials::id.eq(cred_id))
            .filter(credentials::user_id.eq(user.id))
            .filter(credentials::kind.eq("api_key"))
            .select(credentials::id)
            .first(&mut conn)
            .await
            .optional()
            .map_err(|err| AppError::db(err, "models.create.verify_credential"))?;
        if exists.is_none() {
            return Err(AppError::BadRequest("API key credential not found".into()));
        }
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
        capabilities: input.capabilities,
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

    Ok((StatusCode::CREATED, Json(model_response(&inserted))))
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<ModelResponse>>> {
    let mut conn = state.db.get().await?;

    let rows: Vec<ModelConfig> = models::table
        .filter(models::user_id.eq(user.id))
        .order(models::created_at.asc())
        .select(ModelConfig::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "models.list"))?;

    Ok(Json(rows.iter().map(model_response).collect()))
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

    if let Some(capabilities) = &input.capabilities {
        validate_capabilities(capabilities)?;
    }
    if let Some(params) = &input.params {
        validate_params(params)?;
    }

    let mut conn = state.db.get().await?;

    if let Some(Some(cred_id)) = input.credential_id {
        let exists: Option<Uuid> = credentials::table
            .filter(credentials::id.eq(cred_id))
            .filter(credentials::user_id.eq(user.id))
            .filter(credentials::kind.eq("api_key"))
            .select(credentials::id)
            .first(&mut conn)
            .await
            .optional()
            .map_err(|err| AppError::db(err, "models.update.verify_credential"))?;
        if exists.is_none() {
            return Err(AppError::BadRequest("API key credential not found".into()));
        }
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
        capabilities: input.capabilities,
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

    Ok(Json(model_response(&updated)))
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
