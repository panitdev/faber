//! Browsing the read-only model-preset catalog.
//!
//! Presets are not models: nothing here creates, edits, or deletes anything.
//! The catalog is loaded once at boot (see `crates/presets`) and served as-is,
//! so this module is a read surface and nothing more.

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    routes::clamp_limit,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/model-presets", get(list))
        .route("/api/model-presets/providers", get(providers))
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
    items: Vec<presets::Preset>,
}

/// The catalog, or a 503 naming the reason if it was never loaded. A boot
/// whose fetch failed serves every other route; only this surface is missing.
fn catalog(state: &AppState) -> Result<&std::sync::Arc<presets::Catalog>, AppError> {
    state.presets.as_ref().ok_or_else(|| {
        AppError::ServiceUnavailable(
            "the model preset catalog is unavailable; the service could not load it at boot".into(),
        )
    })
}

async fn list(
    State(state): State<AppState>,
    AuthUser(_): AuthUser,
    Query(params): Query<ListQuery>,
) -> ApiResult<Json<PageResponse>> {
    let catalog = catalog(&state)?;

    let query = presets::Query {
        provider: params.provider,
        search: params.q,
        vision: params.vision,
        reasoning: params.reasoning,
        tools: params.tools,
    };

    let matches = catalog.filter(&query);
    let total = matches.len();
    let limit = clamp_limit(params.limit) as usize;
    let offset = params.offset.unwrap_or(0).max(0) as usize;

    let items = matches
        .into_iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();

    Ok(Json(PageResponse {
        total,
        limit,
        offset,
        items,
    }))
}

async fn providers(
    State(state): State<AppState>,
    AuthUser(_): AuthUser,
) -> ApiResult<Json<Vec<presets::Provider>>> {
    let catalog = catalog(&state)?;
    Ok(Json(catalog.providers().to_vec()))
}
