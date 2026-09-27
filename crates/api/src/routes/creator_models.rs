//! Browse routes for creator models.
//!
//! A creator model is a model as the lab that made it describes it, shared by
//! every provider that serves it. They are all the system's — upserted at boot
//! by [`crate::models::model_preset::replace_all`] — so these routes only
//! read. A caller links a preset of their own to one through the preset
//! routes.

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use diesel::{
    BoolExpressionMethods, ExpressionMethods, OptionalExtension, PgTextExpressionMethods, QueryDsl,
    SelectableHelper,
    dsl::sql,
    pg::Pg,
    sql_types::Bool,
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::creator_model::CreatorModelRow,
    routes::{clamp_limit, escape_like},
    schema::{creator_models, model_presets},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/creator-models", get(list))
        .route("/api/creator-models/{id}", get(get_one))
}

/// Absent fields do not filter; a capability given as `false` requires its
/// absence.
#[derive(Deserialize)]
struct ListQuery {
    /// A creator key, e.g. `anthropic`.
    creator: Option<String>,
    /// Case-insensitive substring of the model's id or name.
    q: Option<String>,
    vision: Option<bool>,
    reasoning: Option<bool>,
    tool_call: Option<bool>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct PageResponse {
    total: usize,
    limit: usize,
    offset: usize,
    items: Vec<CreatorModelResponse>,
}

/// A creator model as the client sees it. `creator_model_id` is the row handle
/// a preset links by; the flattened [`presets::CreatorModel`] keeps its `id`
/// as `<creator>/<model>`.
#[derive(Serialize)]
struct CreatorModelResponse {
    creator_model_id: Uuid,
    /// How many presets the caller can see serve this model.
    preset_count: usize,
    #[serde(flatten)]
    model: presets::CreatorModel,
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(params): Query<ListQuery>,
) -> ApiResult<Json<PageResponse>> {
    let mut conn = state.db.get().await?;

    // Built twice, for the count and the page; see `model_presets::list`.
    macro_rules! scoped {
        ($params:expr) => {{
            let mut query = creator_models::table.into_boxed::<Pg>();

            if let Some(creator) = $params.creator.as_deref() {
                query = query.filter(creator_models::creator.eq(creator));
            }
            if let Some(needle) = $params
                .q
                .as_deref()
                .map(str::trim)
                .filter(|needle| !needle.is_empty())
            {
                let pattern = format!("%{}%", escape_like(needle));
                query = query.filter(
                    creator_models::model_id
                        .ilike(pattern.clone())
                        .or(creator_models::name.ilike(pattern)),
                );
            }
            if let Some(vision) = $params.vision {
                query = query.filter(
                    sql::<Bool>(
                        "COALESCE(jsonb_exists(creator_models.modalities -> 'input', 'image'), \
                         false) = ",
                    )
                    .bind::<Bool, _>(vision),
                );
            }
            if let Some(reasoning) = $params.reasoning {
                query = query.filter(creator_models::reasoning.eq(reasoning));
            }
            if let Some(tool_call) = $params.tool_call {
                query = query.filter(creator_models::tool_call.eq(tool_call));
            }
            query
        }};
    }

    let total: i64 = scoped!(&params)
        .count()
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "creator_models.list_count"))?;

    let limit = clamp_limit(params.limit);
    let offset = params.offset.unwrap_or(0).max(0);

    let rows: Vec<CreatorModelRow> = scoped!(&params)
        .order_by(creator_models::model_id.asc())
        .limit(limit)
        .offset(offset)
        .select(CreatorModelRow::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "creator_models.list"))?;

    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let counts = preset_counts(&mut conn, &ids, user.id).await?;

    Ok(Json(PageResponse {
        total: total as usize,
        limit: limit as usize,
        offset: offset as usize,
        items: rows
            .iter()
            .map(|row| CreatorModelResponse {
                creator_model_id: row.id,
                preset_count: counts.get(&row.id).copied().unwrap_or(0),
                model: row.to_creator_model(),
            })
            .collect(),
    }))
}

async fn get_one(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CreatorModelResponse>> {
    let mut conn = state.db.get().await?;

    let row: CreatorModelRow = creator_models::table
        .filter(creator_models::id.eq(id))
        .select(CreatorModelRow::as_select())
        .first(&mut conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "creator_models.get"))?
        .ok_or(AppError::NotFound)?;

    let counts = preset_counts(&mut conn, &[id], user.id).await?;

    Ok(Json(CreatorModelResponse {
        creator_model_id: row.id,
        preset_count: counts.get(&row.id).copied().unwrap_or(0),
        model: row.to_creator_model(),
    }))
}

/// How many presets `owner` can see — their own and the system's — link to
/// each of `ids`.
async fn preset_counts(
    conn: &mut AsyncPgConnection,
    ids: &[Uuid],
    owner: Uuid,
) -> ApiResult<HashMap<Uuid, usize>> {
    let counts: Vec<(Option<Uuid>, i64)> = model_presets::table
        .filter(model_presets::creator_model_id.eq_any(ids))
        .filter(
            model_presets::user_id
                .eq(owner)
                .or(model_presets::user_id.is_null()),
        )
        .group_by(model_presets::creator_model_id)
        .select((model_presets::creator_model_id, diesel::dsl::count_star()))
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "creator_models.preset_counts"))?;

    Ok(counts
        .into_iter()
        .filter_map(|(id, count)| Some((id?, count as usize)))
        .collect())
}
