//! Spawn templates — see `internal-docs/host.md`.
//!
//! An image is not a host, not a container, and not owned by either. It exists
//! so "start me a fresh one" is a one-shot convenience rather than a lifecycle
//! commitment.
//!
//! Faber does now track what it creates — `host_container.managed_at` — and a
//! spawned container records the template it came from. That is provenance and
//! only provenance: nothing resolves through `image_id`, deleting a template
//! nulls it rather than touching the container, and the container's own ref
//! stays the thing execution uses.
//!
//! Only the registration half lives here. Spawning is
//! `POST /api/hosts/{id}/containers/spawn`, on the host — because a container
//! is created on a machine, and the machine is the half that carries a daemon,
//! authentication, and a network path.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{patch, post},
};
use chrono::{DateTime, Utc};
use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::host::{Image, NewImage, UpdateImage},
    routes::deserialize_optional_field,
    schema::image,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/images", post(create).get(list))
        .route("/api/images/{id}", patch(update).delete(remove))
}

#[derive(Deserialize)]
struct CreateRequest {
    name: String,
    reference: String,
    default_mounts: Option<Value>,
    default_root_path: String,
}

#[derive(Deserialize)]
struct UpdateRequest {
    name: Option<String>,
    reference: Option<String>,
    /// `null` explicitly clears the field; omitting it leaves it unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    default_mounts: Option<Option<Value>>,
    default_root_path: Option<String>,
}

#[derive(Serialize)]
struct ImageResponse {
    id: Uuid,
    name: String,
    reference: String,
    default_mounts: Option<Value>,
    default_root_path: String,
    created_at: DateTime<Utc>,
}

fn image_response(i: &Image) -> ImageResponse {
    ImageResponse {
        id: i.id,
        name: i.name.clone(),
        reference: i.reference.clone(),
        default_mounts: i.default_mounts.clone(),
        default_root_path: i.default_root_path.clone(),
        created_at: i.created_at,
    }
}

fn validate_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if name.chars().count() > 100 {
        return Err(AppError::BadRequest(
            "name must be 100 characters or fewer".into(),
        ));
    }
    Ok(())
}

/// The same absolute-path rule containers carry: a relative root would teach
/// the agent host-specific path habits that silently fail to transfer.
fn validate_root_path(root_path: &str) -> Result<(), AppError> {
    if root_path.is_empty() {
        return Err(AppError::BadRequest("default_root_path is required".into()));
    }
    if !root_path.starts_with('/') {
        return Err(AppError::BadRequest(
            "default_root_path must be absolute".into(),
        ));
    }
    Ok(())
}

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateRequest>,
) -> ApiResult<(StatusCode, Json<ImageResponse>)> {
    let name = input.name.trim();
    validate_name(name)?;

    let reference = input.reference.trim();
    if reference.is_empty() {
        return Err(AppError::BadRequest("reference is required".into()));
    }

    let default_root_path = input.default_root_path.trim();
    validate_root_path(default_root_path)?;

    let new_image = NewImage {
        id: Uuid::now_v7(),
        user_id: user.id,
        name,
        reference,
        default_mounts: input.default_mounts,
        default_root_path,
    };

    let mut conn = state.db.get().await?;

    let inserted: Image = diesel::insert_into(image::table)
        .values(&new_image)
        .returning(Image::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("an image named '{name}' already exists")),
            other => AppError::db(other, "images.create"),
        })?;

    Ok((StatusCode::CREATED, Json(image_response(&inserted))))
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<ImageResponse>>> {
    let mut conn = state.db.get().await?;

    let rows: Vec<Image> = image::table
        .filter(image::user_id.eq(user.id))
        .order(image::created_at.asc())
        .select(Image::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "images.list"))?;

    Ok(Json(rows.iter().map(image_response).collect()))
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRequest>,
) -> ApiResult<Json<ImageResponse>> {
    if let Some(name) = input.name.as_deref() {
        validate_name(name.trim())?;
    }

    if let Some(ref reference) = input.reference
        && reference.trim().is_empty()
    {
        return Err(AppError::BadRequest("reference cannot be empty".into()));
    }

    if let Some(root_path) = input.default_root_path.as_deref() {
        validate_root_path(root_path.trim())?;
    }

    let mut conn = state.db.get().await?;

    let patch = UpdateImage {
        name: input.name.as_deref().map(str::trim),
        reference: input.reference.as_deref().map(str::trim),
        default_mounts: input.default_mounts,
        default_root_path: input.default_root_path.as_deref().map(str::trim),
    };

    let updated: Image = diesel::update(
        image::table
            .filter(image::id.eq(id))
            .filter(image::user_id.eq(user.id)),
    )
    .set(patch)
    .returning(Image::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|err| match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => AppError::BadRequest("an image with that name already exists".into()),
        other => AppError::db(other, "images.update"),
    })?;

    Ok(Json(image_response(&updated)))
}

/// Deletes the template only. Containers spawned from it are unaffected —
/// there is no link from a container back to the image it came from.
async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        image::table
            .filter(image::id.eq(id))
            .filter(image::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "images.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}
