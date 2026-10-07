//! Projects, their plugin bindings, and the plugin types this server hosts.
//!
//! | Method | Path | |
//! |---|---|---|
//! | `GET`, `POST` | `/api/projects` | `POST { name, description? }` returns the project with default bindings |
//! | `GET`, `PATCH`, `DELETE` | `/api/projects/{id}` | `PATCH` changes name and description only |
//! | `GET` | `/api/plugin-types` | Registered types with their manifests |
//! | `PUT` | `/api/projects/{id}/plugins/{type}` | Bind, or replace the config; `config-error`s on refusal |
//! | `PATCH` | `/api/projects/{id}/plugins/{type}` | `{ enabled }` |
//! | `DELETE` | `/api/projects/{id}/plugins/{type}` | 409 while another binding imports what it provides |
//! | `GET`, `POST` | `/api/projects/{id}/sessions` | Sessions in the project |
//!
//! Every write takes `If-Match: <rev>` and answers 409 when it does not match;
//! every project response carries the current `rev` and an `ETag` of it.
//! Bind-time refusals reach the user here and never the model.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use chrono::{DateTime, Utc};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::{
    AsyncConnection, AsyncPgConnection, RunQueryDsl, scoped_futures::ScopedFutureExt,
};
use plugin::{BindError, Manifest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    access::personal_workspace,
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{
        now_epoch,
        project::{NewProject, Project, ProjectBinding},
        session::{NewSession, Session},
        thread::{NewThread, Thread},
    },
    routes::{
        clamp_limit,
        sessions::{CreatedSessionResponse, session_response, validate_title},
        threads::thread_response,
    },
    schema::{project, project_binding, session, thread},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects", get(list).post(create))
        .route(
            "/api/projects/{id}",
            get(get_project).patch(update).delete(remove),
        )
        .route("/api/plugin-types", get(plugin_types))
        .route(
            "/api/projects/{id}/plugins/{plugin_type}",
            put(bind).patch(set_enabled).delete(unbind),
        )
        .route(
            "/api/projects/{id}/sessions",
            get(list_sessions).post(create_session),
        )
}

const MAX_NAME_CHARS: usize = 120;
const MAX_DESCRIPTION_CHARS: usize = 4000;

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ProjectResponse {
    id: Uuid,
    name: String,
    description: String,
    owner_id: Uuid,
    rev: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// In creation order, which is tool, notice and head order.
    bindings: Vec<BindingResponse>,
}

#[derive(Serialize)]
struct BindingResponse {
    #[serde(rename = "type")]
    plugin_type: String,
    version: String,
    config_version: i32,
    config: Value,
    enabled: bool,
    created_at: DateTime<Utc>,
}

fn binding_response(row: &ProjectBinding) -> BindingResponse {
    BindingResponse {
        plugin_type: row.plugin_type.clone(),
        version: row.version.clone(),
        config_version: row.config_version,
        config: row.config.clone(),
        enabled: row.enabled,
        created_at: row.created_at,
    }
}

/// The project, with an `ETag` a client sends back as `If-Match`.
fn project_reply(status: StatusCode, found: &Project, bindings: &[ProjectBinding]) -> Response {
    let body = ProjectResponse {
        id: found.id,
        name: found.name.clone(),
        description: found.description.clone(),
        owner_id: found.owner_id,
        rev: found.rev,
        created_at: found.created_at,
        updated_at: found.updated_at,
        bindings: bindings.iter().map(binding_response).collect(),
    };
    let mut response = (status, Json(body)).into_response();
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{}\"", found.rev)) {
        response.headers_mut().insert(header::ETAG, etag);
    }
    response
}

// ---------------------------------------------------------------------------
// Shared steps
// ---------------------------------------------------------------------------

/// The caller's project, or 404 — whether someone else's project exists is
/// not the caller's business.
async fn owned(conn: &mut AsyncPgConnection, user_id: Uuid, id: Uuid) -> ApiResult<Project> {
    project::table
        .filter(project::id.eq(id))
        .filter(project::owner_id.eq(user_id))
        .select(Project::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "projects.owned"))?
        .ok_or(AppError::NotFound)
}

/// [`owned`], locked for the rest of the transaction, with `If-Match`
/// checked against it.
async fn locked(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    id: Uuid,
    expected: Option<i64>,
) -> ApiResult<Project> {
    let found: Project = project::table
        .filter(project::id.eq(id))
        .filter(project::owner_id.eq(user_id))
        .for_update()
        .select(Project::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "projects.locked"))?
        .ok_or(AppError::NotFound)?;
    if let Some(expected) = expected
        && expected != found.rev
    {
        return Err(AppError::Coded {
            status: StatusCode::CONFLICT,
            code: "rev-mismatch",
            message: format!(
                "the project is at rev {}, not {expected}; reload it and try again",
                found.rev
            ),
            details: None,
        });
    }
    Ok(found)
}

/// `If-Match: <rev>`, with or without quotes. Absent is no precondition.
fn if_match(headers: &HeaderMap) -> ApiResult<Option<i64>> {
    let Some(value) = headers.get(header::IF_MATCH) else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| AppError::BadRequest("If-Match is not text".into()))?
        .trim()
        .trim_start_matches("W/")
        .trim_matches('"');
    text.parse()
        .map(Some)
        .map_err(|_| AppError::BadRequest("If-Match must be the project's rev".into()))
}

/// Bumps `rev` and `updated_at` after a change.
async fn touch(conn: &mut AsyncPgConnection, id: Uuid) -> ApiResult<Project> {
    diesel::update(project::table.filter(project::id.eq(id)))
        .set((
            project::rev.eq(project::rev + 1),
            project::updated_at.eq(diesel::dsl::now),
        ))
        .returning(Project::as_returning())
        .get_result(conn)
        .await
        .map_err(|err| AppError::db(err, "projects.touch"))
}

async fn bindings_of(conn: &mut AsyncPgConnection, id: Uuid) -> ApiResult<Vec<ProjectBinding>> {
    project_binding::table
        .filter(project_binding::project_id.eq(id))
        .order((
            project_binding::created_at.asc(),
            project_binding::plugin_type.asc(),
        ))
        .select(ProjectBinding::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "projects.bindings"))
}

/// A name in use for this owner is a conflict, not a server error.
fn name_taken(err: diesel::result::Error, context: &'static str) -> AppError {
    match err {
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => AppError::Coded {
            status: StatusCode::CONFLICT,
            code: "name-taken",
            message: "you already have a project with that name".into(),
            details: None,
        },
        other => AppError::db(other, context),
    }
}

fn check_name(name: &str) -> ApiResult<()> {
    if name.is_empty() {
        return Err(AppError::BadRequest("name cannot be empty".into()));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(AppError::BadRequest(format!(
            "name must be {MAX_NAME_CHARS} characters or fewer"
        )));
    }
    Ok(())
}

fn check_description(description: &str) -> ApiResult<()> {
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(AppError::BadRequest(format!(
            "description must be {MAX_DESCRIPTION_CHARS} characters or fewer"
        )));
    }
    Ok(())
}

/// A bind-time refusal, as the user sees it.
fn bind_error(error: BindError) -> AppError {
    let code = error.code();
    match error {
        BindError::UnknownType(_) | BindError::UnknownVersion { .. } => AppError::Coded {
            status: StatusCode::NOT_FOUND,
            code,
            message: error.to_string(),
            details: None,
        },
        BindError::ConfigInvalid(errors) => AppError::Coded {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code,
            message: "the plugin refused this config".into(),
            details: serde_json::to_value(errors).ok(),
        },
        BindError::LinkRefused(_)
        | BindError::DependencyCycle(_)
        | BindError::DuplicateProvider { .. } => AppError::Coded {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code,
            message: error.to_string(),
            details: None,
        },
        BindError::StillImported(_) => AppError::Coded {
            status: StatusCode::CONFLICT,
            code,
            message: format!("this plugin is {error}; unbind those first"),
            details: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<i64>,
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<ProjectResponse>>> {
    let mut conn = state.db.get().await?;
    let projects: Vec<Project> = project::table
        .filter(project::owner_id.eq(user.id))
        .order(project::updated_at.desc())
        .limit(clamp_limit(query.limit))
        .select(Project::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "projects.list"))?;

    let ids: Vec<Uuid> = projects.iter().map(|found| found.id).collect();
    let mut rows: Vec<ProjectBinding> = project_binding::table
        .filter(project_binding::project_id.eq_any(&ids))
        .order((
            project_binding::created_at.asc(),
            project_binding::plugin_type.asc(),
        ))
        .select(ProjectBinding::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "projects.list.bindings"))?;

    Ok(Json(
        projects
            .into_iter()
            .map(|found| {
                let (mine, rest): (Vec<_>, Vec<_>) =
                    rows.drain(..).partition(|row| row.project_id == found.id);
                rows = rest;
                ProjectResponse {
                    id: found.id,
                    name: found.name,
                    description: found.description,
                    owner_id: found.owner_id,
                    rev: found.rev,
                    created_at: found.created_at,
                    updated_at: found.updated_at,
                    bindings: mine.iter().map(binding_response).collect(),
                }
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct CreateRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

/// A new project gets every built-in plugin bound with its default config.
async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateRequest>,
) -> ApiResult<Response> {
    let name = input.name.trim().to_owned();
    check_name(&name)?;
    let description = input.description.unwrap_or_default().trim().to_owned();
    check_description(&description)?;

    let registry = std::sync::Arc::clone(&state.plugins.registry);
    let mut conn = state.db.get().await?;
    let (created, bindings) = conn
        .transaction::<_, AppError, _>(|conn| {
            async move {
                let id = Uuid::now_v7();
                diesel::insert_into(project::table)
                    .values(&NewProject {
                        id,
                        owner_id: user.id,
                        name: &name,
                        description: &description,
                    })
                    .execute(conn)
                    .await
                    .map_err(|err| name_taken(err, "projects.create"))?;

                // One statement per binding, so each gets its own
                // `clock_timestamp()` and the registry's order is kept.
                for default in registry.defaults() {
                    let manifest = default.manifest();
                    let prepared = plugin::bind::prepare(
                        &registry,
                        &manifest.id,
                        Some(&manifest.version),
                        manifest.config.default.clone(),
                        None,
                    )
                    .map_err(bind_error)?;
                    insert_binding(conn, id, &manifest.id, &prepared).await?;
                }

                let created = owned(conn, user.id, id).await?;
                let bindings = bindings_of(conn, id).await?;
                Ok((created, bindings))
            }
            .scope_boxed()
        })
        .await?;

    Ok(project_reply(StatusCode::CREATED, &created, &bindings))
}

async fn insert_binding(
    conn: &mut AsyncPgConnection,
    project_id: Uuid,
    plugin_type: &str,
    prepared: &plugin::Prepared,
) -> ApiResult<()> {
    diesel::insert_into(project_binding::table)
        .values((
            project_binding::project_id.eq(project_id),
            project_binding::plugin_type.eq(plugin_type),
            project_binding::version.eq(&prepared.version),
            project_binding::config_version.eq(prepared.config_version as i32),
            project_binding::config.eq(&prepared.config),
            project_binding::enabled.eq(true),
        ))
        .execute(conn)
        .await
        .map_err(|err| AppError::db(err, "projects.bind.insert"))?;
    Ok(())
}

async fn get_project(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let mut conn = state.db.get().await?;
    let found = owned(&mut conn, user.id, id).await?;
    let bindings = bindings_of(&mut conn, id).await?;
    Ok(project_reply(StatusCode::OK, &found, &bindings))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRequest {
    name: Option<String>,
    description: Option<String>,
}

/// Name and description only; bindings have their own routes.
async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<UpdateRequest>,
) -> ApiResult<Response> {
    let expected = if_match(&headers)?;
    let name = input.name.map(|name| name.trim().to_owned());
    if let Some(name) = &name {
        check_name(name)?;
    }
    let description = input.description.map(|text| text.trim().to_owned());
    if let Some(description) = &description {
        check_description(description)?;
    }

    let mut conn = state.db.get().await?;
    let (updated, bindings) = conn
        .transaction::<_, AppError, _>(|conn| {
            async move {
                locked(conn, user.id, id, expected).await?;
                if let Some(name) = &name {
                    diesel::update(project::table.filter(project::id.eq(id)))
                        .set(project::name.eq(name))
                        .execute(conn)
                        .await
                        .map_err(|err| name_taken(err, "projects.update.name"))?;
                }
                if let Some(description) = &description {
                    diesel::update(project::table.filter(project::id.eq(id)))
                        .set(project::description.eq(description))
                        .execute(conn)
                        .await
                        .map_err(|err| AppError::db(err, "projects.update.description"))?;
                }
                let updated = touch(conn, id).await?;
                let bindings = bindings_of(conn, id).await?;
                Ok((updated, bindings))
            }
            .scope_boxed()
        })
        .await?;

    Ok(project_reply(StatusCode::OK, &updated, &bindings))
}

/// Deletes the project, its bindings, and by cascade every session in it.
async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    let expected = if_match(&headers)?;
    let mut conn = state.db.get().await?;
    conn.transaction::<_, AppError, _>(|conn| {
        async move {
            locked(conn, user.id, id, expected).await?;
            diesel::delete(project::table.filter(project::id.eq(id)))
                .execute(conn)
                .await
                .map_err(|err| AppError::db(err, "projects.delete"))?;
            Ok(())
        }
        .scope_boxed()
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Plugin types and bindings
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct PluginTypeResponse {
    #[serde(flatten)]
    manifest: Manifest,
    /// Interfaces this type provides to other bindings.
    exports: Vec<String>,
    /// Interfaces it needs a provider for.
    imports: Vec<String>,
    /// Bound to new projects.
    default: bool,
}

/// Registered types with their manifests: tools, capabilities, config schema
/// and default.
async fn plugin_types(
    State(state): State<AppState>,
    AuthUser(_user): AuthUser,
) -> Json<Vec<PluginTypeResponse>> {
    let registry = &state.plugins.registry;
    let defaults: Vec<String> = registry
        .defaults()
        .iter()
        .map(|found| found.manifest().id.clone())
        .collect();
    Json(
        registry
            .manifests()
            .into_iter()
            .map(|manifest| {
                let found = registry
                    .get(&manifest.id, &manifest.version)
                    .expect("a listed manifest is registered");
                PluginTypeResponse {
                    exports: found.exports(),
                    imports: found.imports(),
                    default: defaults.contains(&manifest.id),
                    manifest,
                }
            })
            .collect(),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindRequest {
    /// The plugin's config. Absent keeps the current one, or binds the
    /// manifest's default.
    config: Option<Value>,
    /// The config version `config` was written under; absent is the
    /// manifest's current one.
    config_version: Option<u32>,
    /// The plugin version to bind; absent keeps the bound one, or binds the
    /// newest. Naming a newer one upgrades.
    version: Option<String>,
}

/// Binds a plugin type, or replaces its config (or version). Runs `migrate`
/// and `validate`; refuses a config either rejects, a second provider of an
/// interface, and an import cycle.
async fn bind(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, plugin_type)): Path<(Uuid, String)>,
    headers: HeaderMap,
    body: Option<Json<BindRequest>>,
) -> ApiResult<Response> {
    let expected = if_match(&headers)?;
    let input = body.map(|Json(input)| input).unwrap_or(BindRequest {
        config: None,
        config_version: None,
        version: None,
    });
    let registry = std::sync::Arc::clone(&state.plugins.registry);

    let mut conn = state.db.get().await?;
    let (status, updated, bindings) = conn
        .transaction::<_, AppError, _>(|conn| {
            async move {
                locked(conn, user.id, id, expected).await?;
                let mut rows = bindings_of(conn, id).await?;
                let existing = rows
                    .iter()
                    .find(|row| row.plugin_type == plugin_type)
                    .cloned();

                let latest = registry
                    .latest(&plugin_type)
                    .ok_or_else(|| bind_error(BindError::UnknownType(plugin_type.clone())))?;
                let version = input
                    .version
                    .clone()
                    .or_else(|| existing.as_ref().map(|row| row.version.clone()))
                    .unwrap_or_else(|| latest.manifest().version.clone());
                let (config, config_version) = match (&input.config, &existing) {
                    (Some(config), _) => (config.clone(), input.config_version),
                    (None, Some(row)) => {
                        (row.config.clone(), Some(row.config_version.max(1) as u32))
                    }
                    (None, None) => (latest.manifest().config.default.clone(), None),
                };
                let prepared = plugin::bind::prepare(
                    &registry,
                    &plugin_type,
                    Some(&version),
                    config,
                    config_version,
                )
                .map_err(bind_error)?;

                // The graph as it would be after this change.
                let mut after: Vec<plugin::Binding> = rows
                    .iter()
                    .filter(|row| row.plugin_type != plugin_type)
                    .map(ProjectBinding::to_binding)
                    .collect();
                after.push(plugin::Binding {
                    plugin: plugin_type.clone(),
                    version: prepared.version.clone(),
                    config_version: prepared.config_version,
                    config: prepared.config.clone(),
                    enabled: existing.as_ref().is_none_or(|row| row.enabled),
                    order: existing
                        .as_ref()
                        .map_or(i64::MAX, |row| row.created_at.timestamp_micros()),
                });
                plugin::bind::check_graph(&registry, &after).map_err(bind_error)?;

                let status = if existing.is_some() {
                    diesel::update(
                        project_binding::table
                            .filter(project_binding::project_id.eq(id))
                            .filter(project_binding::plugin_type.eq(&plugin_type)),
                    )
                    .set((
                        project_binding::version.eq(&prepared.version),
                        project_binding::config_version.eq(prepared.config_version as i32),
                        project_binding::config.eq(&prepared.config),
                    ))
                    .execute(conn)
                    .await
                    .map_err(|err| AppError::db(err, "projects.bind.update"))?;
                    StatusCode::OK
                } else {
                    insert_binding(conn, id, &plugin_type, &prepared).await?;
                    StatusCode::CREATED
                };

                let updated = touch(conn, id).await?;
                rows = bindings_of(conn, id).await?;
                Ok((status, updated, rows))
            }
            .scope_boxed()
        })
        .await?;

    Ok(project_reply(status, &updated, &bindings))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnabledRequest {
    enabled: bool,
}

/// Turns a binding off or on. A disabled provider degrades its importers at
/// their next run start rather than being refused here.
async fn set_enabled(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, plugin_type)): Path<(Uuid, String)>,
    headers: HeaderMap,
    Json(input): Json<EnabledRequest>,
) -> ApiResult<Response> {
    let expected = if_match(&headers)?;
    let mut conn = state.db.get().await?;
    let (updated, bindings) = conn
        .transaction::<_, AppError, _>(|conn| {
            async move {
                locked(conn, user.id, id, expected).await?;
                let changed = diesel::update(
                    project_binding::table
                        .filter(project_binding::project_id.eq(id))
                        .filter(project_binding::plugin_type.eq(&plugin_type)),
                )
                .set(project_binding::enabled.eq(input.enabled))
                .execute(conn)
                .await
                .map_err(|err| AppError::db(err, "projects.enable"))?;
                if changed == 0 {
                    return Err(AppError::NotFound);
                }
                let updated = touch(conn, id).await?;
                let bindings = bindings_of(conn, id).await?;
                Ok((updated, bindings))
            }
            .scope_boxed()
        })
        .await?;
    Ok(project_reply(StatusCode::OK, &updated, &bindings))
}

/// Unbinds a plugin type. Refused while another binding imports an interface
/// it provides.
async fn unbind(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((id, plugin_type)): Path<(Uuid, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let expected = if_match(&headers)?;
    let registry = std::sync::Arc::clone(&state.plugins.registry);
    let mut conn = state.db.get().await?;
    let (updated, bindings) = conn
        .transaction::<_, AppError, _>(|conn| {
            async move {
                locked(conn, user.id, id, expected).await?;
                let rows = bindings_of(conn, id).await?;
                if !rows.iter().any(|row| row.plugin_type == plugin_type) {
                    return Err(AppError::NotFound);
                }
                let all: Vec<plugin::Binding> =
                    rows.iter().map(ProjectBinding::to_binding).collect();
                let importers = plugin::bind::importers(&registry, &all, &plugin_type);
                if !importers.is_empty() {
                    return Err(bind_error(BindError::StillImported(importers)));
                }
                diesel::delete(
                    project_binding::table
                        .filter(project_binding::project_id.eq(id))
                        .filter(project_binding::plugin_type.eq(&plugin_type)),
                )
                .execute(conn)
                .await
                .map_err(|err| AppError::db(err, "projects.unbind"))?;
                let updated = touch(conn, id).await?;
                let bindings = bindings_of(conn, id).await?;
                Ok((updated, bindings))
            }
            .scope_boxed()
        })
        .await?;
    Ok(project_reply(StatusCode::OK, &updated, &bindings))
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateSessionRequest {
    title: Option<String>,
}

/// A session in the project. It lands in the owner's personal workspace, so
/// every session route reaches it as usual; its runs go through the project's
/// plugins.
async fn create_session(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    body: Option<Json<CreateSessionRequest>>,
) -> ApiResult<(StatusCode, Json<CreatedSessionResponse>)> {
    let input = body.map(|Json(input)| input).unwrap_or_default();
    let title = input.title.as_deref().map(str::trim);
    if let Some(title) = title {
        validate_title(title)?;
    }

    let mut conn = state.db.get().await?;
    owned(&mut conn, user.id, id).await?;
    let workspace = personal_workspace(&mut conn, user.id).await?;
    let now = now_epoch();
    let session_id = Uuid::now_v7();

    let (created, root) = conn
        .transaction::<_, AppError, _>(|conn| {
            async move {
                let created: Session = diesel::insert_into(session::table)
                    .values(&NewSession {
                        id: session_id,
                        workspace_id: workspace.id,
                        title,
                        created_at: now,
                        project_id: Some(id),
                    })
                    .returning(Session::as_returning())
                    .get_result(conn)
                    .await
                    .map_err(|err| AppError::db(err, "projects.sessions.insert"))?;
                let root: Thread = diesel::insert_into(thread::table)
                    .values(&NewThread {
                        id: Uuid::now_v7(),
                        session_id,
                        parent_id: None,
                        forked_at_seq: None,
                        created_at: now,
                    })
                    .returning(Thread::as_returning())
                    .get_result(conn)
                    .await
                    .map_err(|err| AppError::db(err, "projects.sessions.insert_root_thread"))?;
                Ok((created, root))
            }
            .scope_boxed()
        })
        .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedSessionResponse {
            session: session_response(&created),
            root_thread: thread_response(&root),
            default_environments: Vec::new(),
        }),
    ))
}

async fn list_sessions(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<super::sessions::SessionResponse>>> {
    let mut conn = state.db.get().await?;
    owned(&mut conn, user.id, id).await?;
    let rows: Vec<Session> = session::table
        .filter(session::project_id.eq(id))
        .order(session::created_at.desc())
        .limit(clamp_limit(query.limit))
        .select(Session::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "projects.sessions.list"))?;
    Ok(Json(rows.iter().map(session_response).collect()))
}
