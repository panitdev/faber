mod credentials;
mod environments;
mod hosts;
mod images;
mod models;
mod runs;
mod sessions;
mod threads;
mod workspaces;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    agent,
    auth::{AuthUser, extract_token},
    models::user::User,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/api/config", get(config))
        .route("/api/me", get(me))
        .route("/api/logout", post(logout))
        .merge(credentials::router())
        .merge(models::router())
        .merge(hosts::router())
        .merge(environments::router())
        .merge(images::router())
        .merge(workspaces::router())
        .merge(sessions::router())
        .merge(threads::router())
        .merge(runs::router())
        .merge(agent::routes::router())
}

/// Default and ceiling for `?limit=` on collection routes. Unbounded list endpoints are
/// a denial-of-service surface the moment a session accumulates history.
const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

#[derive(Debug, Serialize)]
struct ConfigResponse {
    allow_local_hosts: bool,
}

async fn config(State(state): State<AppState>) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        allow_local_hosts: state.config.allow_local_hosts,
    })
}

pub(crate) fn clamp_limit(requested: Option<i64>) -> i64 {
    requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Distinguishes an absent key from an explicit `null`, so a `PATCH` can clear a nullable
/// column without a sentinel value.
pub(crate) fn deserialize_optional_field<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(de)?))
}

/// Revokes the caller's Surge session (best-effort) and clears the `surge_session` cookie
/// for the configured cookie domain. Unauthenticated calls are a no-op success — logout
/// should never itself require being logged in.
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(raw_token) = extract_token(&headers)
        && let Some(token) = surge::SessionToken::from_raw(&raw_token)
        && let Err(error) = state.auth.revoke_session(&token).await
    {
        tracing::warn!(error = %error, "failed to revoke surge session during logout");
    }

    let clear_cookie = format!(
        "surge_session=; Domain={}; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax",
        state.config.surge_cookie_domain
    );

    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, clear_cookie)])
}

/// Local user identity only — username/display_name/avatar_url live in Surge; callers
/// resolve those via Surge's own whoami rather than through this API.
#[derive(Serialize)]
struct MeResponse {
    id: String,
}

async fn me(State(_state): State<AppState>, AuthUser(user): AuthUser) -> Json<MeResponse> {
    Json(me_response(&user))
}

fn me_response(user: &User) -> MeResponse {
    MeResponse {
        id: user.id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::ConfigResponse;

    /// Axum validates path syntax when the route is registered, not when the crate is
    /// compiled — a v0.7-style `:id` capture type-checks and then panics at boot. Building
    /// the router here moves that failure into `cargo test`.
    #[test]
    fn router_builds() {
        let _ = super::router();
    }

    #[test]
    fn config_response_exposes_only_the_local_host_policy() {
        let value = serde_json::to_value(ConfigResponse {
            allow_local_hosts: false,
        })
        .unwrap();
        assert_eq!(value, serde_json::json!({ "allow_local_hosts": false }));
    }
}
