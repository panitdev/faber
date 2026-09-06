//! Execution environments — see `internal-docs/host.md`.
//!
//! Two things this module deliberately does *not* expose:
//!
//! * **No "is it up" field, and no probe-now route.** Reachability is answered
//!   by the connection attempt. `last_probe` is named for what it is — the most
//!   recent *observation* — so a caller rendering it says "last reachable 3h
//!   ago" rather than showing a status light. The reach-the-machine layer now
//!   exists, which makes a probe-now route buildable and no more useful: its
//!   answer would still be stale by the time anything acted on it.
//! * **No container lifecycle the agent can reach.** Nothing a model calls
//!   creates, starts, stops, or removes a container — an agent that can
//!   restart its own container can destroy the state a user is mid-way through
//!   inspecting. Lifecycle a *user* asks for is different, and `POST
//!   /api/hosts/{id}/containers/spawn` is it: faber creates the container,
//!   records that it did, and may therefore destroy it again on request.
//!   Containers faber only registered are still untouched by `DELETE`, which
//!   ends the registration and leaves the container alone.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use chrono::{DateTime, Utc};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use environment::docker::{Daemon, engine};
use environment::{Denial, Fault};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::host::{
        ExecMode, Host, HostContainer, HostProbe, Image, NewHost, NewHostContainer, NewHostProbe,
        Transport, UpdateHost, UpdateHostContainer,
    },
    routes::{clamp_limit, deserialize_optional_field},
    schema::{credentials, host, host_container, host_probe, image},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/hosts", post(create).get(list))
        .route("/api/hosts/{id}", get(fetch).patch(update).delete(remove))
        .route(
            "/api/hosts/{id}/containers",
            post(create_container).get(list_containers),
        )
        .route("/api/hosts/{id}/containers/spawn", post(spawn_container))
        .route(
            "/api/hosts/{id}/probes",
            post(record_probe).get(list_probes),
        )
        .route(
            "/api/host-containers/{id}",
            patch(update_container).delete(unregister_container),
        )
}

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateHostRequest {
    name: String,
    transport: Transport,
    exec_mode: ExecMode,
    ssh_address: Option<String>,
    ssh_key_ref: Option<String>,
    /// The fingerprint this host is known by. Omitted on first registration:
    /// the first successful connection records what it saw, and everything
    /// after verifies against it.
    ssh_host_key: Option<String>,
    docker_endpoint: Option<String>,
    /// The agent-visible root for direct execution. Absent means this host
    /// cannot be bound to a session directly — only containers on it can.
    root_path: Option<String>,
}

#[derive(Deserialize)]
struct UpdateHostRequest {
    name: Option<String>,
    transport: Option<Transport>,
    exec_mode: Option<ExecMode>,
    /// `null` explicitly clears the field; omitting it leaves it unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    ssh_address: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    ssh_key_ref: Option<Option<String>>,
    /// `null` clears it, which is how a rebuilt machine is re-trusted — an
    /// operator decision, never an automatic one on mismatch.
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    ssh_host_key: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    docker_endpoint: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    root_path: Option<Option<String>>,
    /// Operator intent: `true` stamps `disabled_at`, `false` clears it. Never
    /// an observation — a host nobody can reach is still enabled.
    disabled: Option<bool>,
}

#[derive(Serialize)]
struct HostResponse {
    id: Uuid,
    name: String,
    transport: String,
    exec_mode: String,
    ssh_address: Option<String>,
    ssh_key_ref: Option<String>,
    /// Null until first contact. A public fingerprint, not a secret — it is
    /// what a user checks against what their machine reports.
    ssh_host_key: Option<String>,
    docker_endpoint: Option<String>,
    /// Null when this host can only be reached through its containers.
    root_path: Option<String>,
    created_at: DateTime<Utc>,
    disabled_at: Option<DateTime<Utc>>,
    /// Registrations that have not been unregistered, oldest first.
    containers: Vec<ContainerResponse>,
    /// The most recent observation, or `null` if this host has never been
    /// probed. Advisory: it describes a past attempt, not present reachability.
    last_probe: Option<ProbeResponse>,
}

#[derive(Serialize)]
struct ContainerResponse {
    id: Uuid,
    host_id: Uuid,
    container_ref: String,
    name: Option<String>,
    root_path: String,
    created_at: DateTime<Utc>,
    unregistered_at: Option<DateTime<Utc>>,
    /// Whether faber created this container. A client renders the two
    /// differently on purpose: unregistering a managed container can also
    /// destroy it, and unregistering a registered one never can.
    managed: bool,
    managed_at: Option<DateTime<Utc>>,
    /// The template it came from, or `null` — including when the template was
    /// deleted afterwards. Provenance, never used to resolve anything.
    image_id: Option<Uuid>,
}

#[derive(Serialize)]
struct ProbeResponse {
    id: Uuid,
    host_id: Uuid,
    container_id: Option<Uuid>,
    probed_at: DateTime<Utc>,
    ok: bool,
    error: Option<String>,
    os: Option<String>,
    arch: Option<String>,
    shell: Option<String>,
    tools: Option<Value>,
    root_path: Option<String>,
}

fn container_response(c: &HostContainer) -> ContainerResponse {
    ContainerResponse {
        id: c.id,
        host_id: c.host_id,
        container_ref: c.container_ref.clone(),
        name: c.name.clone(),
        root_path: c.root_path.clone(),
        created_at: c.created_at,
        unregistered_at: c.unregistered_at,
        managed: c.managed(),
        managed_at: c.managed_at,
        image_id: c.image_id,
    }
}

fn probe_response(p: &HostProbe) -> ProbeResponse {
    ProbeResponse {
        id: p.id,
        host_id: p.host_id,
        container_id: p.container_id,
        probed_at: p.probed_at,
        ok: p.ok,
        error: p.error.clone(),
        os: p.os.clone(),
        arch: p.arch.clone(),
        shell: p.shell.clone(),
        tools: p.tools.clone(),
        root_path: p.root_path.clone(),
    }
}

fn host_response(
    h: &Host,
    containers: Vec<ContainerResponse>,
    last_probe: Option<ProbeResponse>,
) -> HostResponse {
    HostResponse {
        id: h.id,
        name: h.name.clone(),
        transport: h.transport.clone(),
        exec_mode: h.exec_mode.clone(),
        ssh_address: h.ssh_address.clone(),
        ssh_key_ref: h.ssh_key_ref.clone(),
        ssh_host_key: h.ssh_host_key.clone(),
        docker_endpoint: h.docker_endpoint.clone(),
        root_path: h.root_path.clone(),
        created_at: h.created_at,
        disabled_at: h.disabled_at,
        containers,
        last_probe,
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

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

/// Mirrors the `host_transport_config` CHECK, so a mismatch comes back as a
/// message naming the field rather than as an opaque constraint violation.
fn validate_transport_config(
    transport: Transport,
    ssh_address: Option<&str>,
    ssh_host_key: Option<&str>,
    allow_local_hosts: bool,
) -> Result<(), AppError> {
    if transport == Transport::Local && !allow_local_hosts {
        return Err(AppError::BadRequest(
            "local hosts are disabled by FABER_ALLOW_LOCAL_HOSTS".into(),
        ));
    }

    match transport {
        Transport::Ssh if ssh_address.is_none_or(str::is_empty) => Err(AppError::BadRequest(
            "ssh_address is required when transport is 'ssh'".into(),
        )),
        Transport::Ssh => validate_ssh_address(ssh_address.expect("checked above")),
        // Agent mode carries no ssh_address (faber never dials it) and
        // no ssh_host_key (the daemon's key is pinned in `agent_credential`
        // at enrollment, not learned here).
        Transport::Local | Transport::Agent if ssh_address.is_some_and(|a| !a.is_empty()) => Err(
            AppError::BadRequest("ssh_address is only valid when transport is 'ssh'".into()),
        ),
        Transport::Local | Transport::Agent if ssh_host_key.is_some_and(|k| !k.is_empty()) => Err(
            AppError::BadRequest("ssh_host_key is only valid when transport is 'ssh'".into()),
        ),
        _ => Ok(()),
    }
}

fn validate_ssh_address(address: &str) -> Result<(), AppError> {
    let (user, host_port) = address.split_once('@').ok_or_else(|| {
        AppError::BadRequest("ssh_address must use the format user@host:port".into())
    })?;
    if user.is_empty() || host_port.is_empty() {
        return Err(AppError::BadRequest(
            "ssh_address must use the format user@host:port".into(),
        ));
    }

    let port = host_port
        .rsplit_once(':')
        .map(|(_, port)| port)
        .filter(|port| !port.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("ssh_address must use the format user@host:port".into())
        })?;
    if port.parse::<u16>().is_err() {
        return Err(AppError::BadRequest(
            "ssh_address port must be between 1 and 65535".into(),
        ));
    }

    Ok(())
}

fn validate_docker_endpoint(
    transport: Transport,
    exec_mode: ExecMode,
    endpoint: Option<&str>,
) -> Result<(), AppError> {
    if exec_mode != ExecMode::Docker {
        return Ok(());
    }

    // Local Docker access is explicit: Faber must not silently use the
    // operator's ambient Docker context or socket.
    if transport == Transport::Local && endpoint.is_none_or(str::is_empty) {
        return Err(AppError::BadRequest(
            "docker_endpoint is required for a local docker host".into(),
        ));
    }

    if let Some(endpoint) = endpoint
        && !endpoint.starts_with("unix://")
        && !endpoint.starts_with("tcp://")
    {
        return Err(AppError::BadRequest(
            "docker_endpoint must start with 'unix://' or 'tcp://'".into(),
        ));
    }

    Ok(())
}

/// The same absolute-path rule a container's root carries, and for the same
/// reason: the whole of the path contract rests on one rooted namespace per
/// target, and a relative root would teach the agent host-specific path habits
/// that silently fail to transfer.
fn validate_host_root(root_path: &str) -> Result<(), AppError> {
    if !root_path.starts_with('/') {
        return Err(AppError::BadRequest("root_path must be absolute".into()));
    }
    Ok(())
}

/// `Some("")` means the caller sent whitespace, which is not a value — it
/// collapses to `None` so an empty string never reaches a nullable column.
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

async fn validate_ssh_credential(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    key_ref: Option<&str>,
) -> Result<(), AppError> {
    let key_ref = key_ref.ok_or_else(|| {
        AppError::BadRequest("ssh_key_ref is required when transport is 'ssh'".into())
    })?;
    let credential_id = Uuid::parse_str(key_ref)
        .map_err(|_| AppError::BadRequest("ssh_key_ref is not a credential id".into()))?;
    let exists: Option<Uuid> = credentials::table
        .filter(credentials::id.eq(credential_id))
        .filter(credentials::user_id.eq(user_id))
        .filter(credentials::kind.eq("ssh_key"))
        .select(credentials::id)
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "hosts.verify_ssh_credential"))?;
    if exists.is_none() {
        return Err(AppError::BadRequest("SSH key credential not found".into()));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Loads a host the caller owns, or `NotFound`.
///
/// Every host is owned, so every route — reads and writes alike — goes
/// through here and nothing else. A host somebody else owns is
/// indistinguishable from one that does not exist.
async fn owned_host(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    id: Uuid,
) -> Result<Host, AppError> {
    host::table
        .filter(host::id.eq(id))
        .filter(host::user_id.eq(user_id))
        .select(Host::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "hosts.load"))?
        .ok_or(AppError::NotFound)
}

/// The caller's active registrations on a set of hosts, oldest first.
///
/// Scoped by the *container's* owner rather than the host's, so
/// authorization reads the container row rather than joining through its
/// host.
async fn containers_for(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    host_ids: &[Uuid],
) -> Result<Vec<HostContainer>, AppError> {
    if host_ids.is_empty() {
        return Ok(Vec::new());
    }
    host_container::table
        .filter(host_container::host_id.eq_any(host_ids))
        .filter(host_container::user_id.eq(user_id))
        .filter(host_container::unregistered_at.is_null())
        .order(host_container::created_at.asc())
        .select(HostContainer::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.containers"))
}

/// The newest probe per host, in one round trip. `DISTINCT ON` keeps this from
/// dragging the whole append-only log across the wire just to read its tail.
async fn latest_probes(
    conn: &mut diesel_async::AsyncPgConnection,
    host_ids: &[Uuid],
) -> Result<Vec<HostProbe>, AppError> {
    if host_ids.is_empty() {
        return Ok(Vec::new());
    }
    host_probe::table
        .filter(host_probe::host_id.eq_any(host_ids))
        .distinct_on(host_probe::host_id)
        .order((host_probe::host_id, host_probe::probed_at.desc()))
        .select(HostProbe::as_select())
        .load(conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.latest_probes"))
}

/// The newest probe for one host, or `None` if it has never been probed.
async fn latest_probe(
    conn: &mut diesel_async::AsyncPgConnection,
    host_id: Uuid,
) -> Result<Option<HostProbe>, AppError> {
    Ok(latest_probes(conn, &[host_id]).await?.into_iter().next())
}

// ---------------------------------------------------------------------------
// Hosts
// ---------------------------------------------------------------------------

async fn create(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Json(input): Json<CreateHostRequest>,
) -> ApiResult<(StatusCode, Json<HostResponse>)> {
    let name = input.name.trim();
    validate_name(name)?;

    let ssh_address = trimmed(input.ssh_address.as_deref());
    let ssh_host_key = trimmed(input.ssh_host_key.as_deref());
    validate_transport_config(
        input.transport,
        ssh_address,
        ssh_host_key,
        state.config.allow_local_hosts,
    )?;

    let host_root = trimmed(input.root_path.as_deref());
    if let Some(root) = host_root {
        validate_host_root(root)?;
    }
    let docker_endpoint = trimmed(input.docker_endpoint.as_deref());
    validate_docker_endpoint(input.transport, input.exec_mode, docker_endpoint)?;

    let ssh_key_ref = trimmed(input.ssh_key_ref.as_deref());
    let mut conn = state.db.get().await?;
    if input.transport == Transport::Ssh {
        validate_ssh_credential(&mut conn, user.id, ssh_key_ref).await?;
    }

    let new_host = NewHost {
        id: Uuid::now_v7(),
        user_id: user.id,
        name,
        transport: input.transport.as_str(),
        exec_mode: input.exec_mode.as_str(),
        ssh_address,
        ssh_key_ref,
        ssh_host_key,
        docker_endpoint,
        root_path: host_root,
        preview_network: None,
    };

    let inserted: Host = diesel::insert_into(host::table)
        .values(&new_host)
        .returning(Host::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!("a host named '{name}' already exists")),
            other => AppError::db(other, "hosts.create"),
        })?;

    // Freshly created: no registrations and nothing observed yet.
    Ok((
        StatusCode::CREATED,
        Json(host_response(&inserted, Vec::new(), None)),
    ))
}

async fn list(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
) -> ApiResult<Json<Vec<HostResponse>>> {
    let mut conn = state.db.get().await?;

    let hosts: Vec<Host> = host::table
        .filter(host::user_id.eq(user.id))
        .order(host::created_at.asc())
        .select(Host::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.list"))?;

    let ids: Vec<Uuid> = hosts.iter().map(|h| h.id).collect();
    let containers = containers_for(&mut conn, user.id, &ids).await?;
    let probes = latest_probes(&mut conn, &ids).await?;

    let mut responses = Vec::with_capacity(hosts.len());
    for h in &hosts {
        responses.push(host_response(
            h,
            containers
                .iter()
                .filter(|c| c.host_id == h.id)
                .map(container_response)
                .collect(),
            probes
                .iter()
                .find(|p| p.host_id == h.id)
                .map(probe_response),
        ));
    }

    Ok(Json(responses))
}

async fn fetch(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<HostResponse>> {
    let mut conn = state.db.get().await?;
    let found = owned_host(&mut conn, user.id, id).await?;

    let containers = containers_for(&mut conn, user.id, &[found.id]).await?;
    let probe = latest_probe(&mut conn, found.id).await?;

    Ok(Json(host_response(
        &found,
        containers.iter().map(container_response).collect(),
        probe.as_ref().map(probe_response),
    )))
}

async fn update(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateHostRequest>,
) -> ApiResult<Json<HostResponse>> {
    let mut conn = state.db.get().await?;
    let current = owned_host(&mut conn, user.id, id).await?;

    if let Some(name) = input.name.as_deref() {
        validate_name(name.trim())?;
    }
    if let Some(Some(root)) = input.root_path.as_ref().map(|v| trimmed(v.as_deref())) {
        validate_host_root(root)?;
    }

    // The sparse-column rule spans two fields, so it has to be checked against
    // the row the patch will produce, not against the patch alone: clearing
    // `ssh_address` is fine on a local host and a 400 on an ssh one.
    let effective_transport = match input.transport {
        Some(t) => t,
        None if current.transport == Transport::Ssh.as_str() => Transport::Ssh,
        None if current.transport == Transport::Agent.as_str() => Transport::Agent,
        None => Transport::Local,
    };
    let effective_ssh_address = match input.ssh_address {
        Some(ref value) => trimmed(value.as_deref()),
        None => current.ssh_address.as_deref(),
    };
    // A host key the caller is *setting* has to be checked; one already stored
    // on a host being switched to local is cleared below rather than refused.
    let submitted_host_key = input
        .ssh_host_key
        .as_ref()
        .and_then(|value| trimmed(value.as_deref()));
    validate_transport_config(
        effective_transport,
        effective_ssh_address,
        submitted_host_key,
        state.config.allow_local_hosts || current.transport == Transport::Local.as_str(),
    )?;

    let effective_ssh_key_ref = match input.ssh_key_ref.as_ref() {
        Some(value) => trimmed(value.as_deref()),
        None => current.ssh_key_ref.as_deref(),
    };
    if effective_transport == Transport::Ssh {
        validate_ssh_credential(&mut conn, user.id, effective_ssh_key_ref).await?;
    }

    let effective_exec_mode = input.exec_mode.unwrap_or_else(|| {
        if current.exec_mode == ExecMode::Docker.as_str() {
            ExecMode::Docker
        } else {
            ExecMode::Direct
        }
    });
    let effective_docker_endpoint = match input.docker_endpoint.as_ref() {
        Some(ref value) => trimmed(value.as_deref()),
        None => current.docker_endpoint.as_deref(),
    };
    validate_docker_endpoint(
        effective_transport,
        effective_exec_mode,
        effective_docker_endpoint,
    )?;

    // A transport switch that leaves the old address behind would fail the DB
    // check, so normalize: going local clears the address alongside it.
    let ssh_address_patch = match (input.transport, input.ssh_address.as_ref()) {
        (_, Some(value)) => Some(trimmed(value.as_deref())),
        (Some(Transport::Local), None) => Some(None),
        _ => None,
    };

    let ssh_key_ref_patch = match (input.transport, input.ssh_key_ref.as_ref()) {
        (_, Some(value)) => Some(trimmed(value.as_deref())),
        (Some(Transport::Local), None) => Some(None),
        _ => None,
    };

    // The host key travels with the address, for the same reason and one more:
    // a host that stops being an ssh host has no key, and a host that changes
    // address is a different machine whose stored fingerprint is now a claim
    // about somewhere else. Keeping it would verify the next connection
    // against the wrong host and refuse it for the wrong reason.
    let ssh_host_key_patch = match (
        input.transport,
        input.ssh_host_key.as_ref(),
        ssh_address_patch,
    ) {
        (_, Some(value), _) => Some(trimmed(value.as_deref())),
        (Some(Transport::Local), None, _) => Some(None),
        (_, None, Some(address)) if address != current.ssh_address.as_deref() => Some(None),
        _ => None,
    };

    let name_trimmed = input.name.as_deref().map(str::trim);
    let patch = UpdateHost {
        name: name_trimmed,
        transport: input.transport.map(|t| t.as_str()),
        exec_mode: input.exec_mode.map(|m| m.as_str()),
        ssh_address: ssh_address_patch,
        ssh_key_ref: ssh_key_ref_patch,
        ssh_host_key: ssh_host_key_patch,
        docker_endpoint: input
            .docker_endpoint
            .as_ref()
            .map(|v| trimmed(v.as_deref())),
        root_path: input.root_path.as_ref().map(|v| trimmed(v.as_deref())),
        disabled_at: input.disabled.map(|d| d.then(Utc::now)),
        ..Default::default()
    };

    let updated: Host = diesel::update(
        host::table
            .filter(host::id.eq(id))
            .filter(host::user_id.eq(user.id)),
    )
    .set(patch)
    .returning(Host::as_returning())
    .get_result(&mut conn)
    .await
    .map_err(|err| match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        ) => AppError::BadRequest("a host with that name already exists".into()),
        other => AppError::db(other, "hosts.update"),
    })?;

    let containers = containers_for(&mut conn, user.id, &[updated.id]).await?;
    let probe = latest_probe(&mut conn, updated.id).await?;

    Ok(Json(host_response(
        &updated,
        containers.iter().map(container_response).collect(),
        probe.as_ref().map(probe_response),
    )))
}

/// Drops the registration and, by cascade, its containers and probe history.
/// Nothing on the machine itself is touched. `PATCH { disabled: true }` is the
/// reversible alternative.
async fn remove(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;

    let deleted = diesel::delete(
        host::table
            .filter(host::id.eq(id))
            .filter(host::user_id.eq(user.id)),
    )
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "hosts.delete"))?;

    if deleted == 0 {
        return Err(AppError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateContainerRequest {
    container_ref: String,
    name: Option<String>,
    root_path: String,
}

#[derive(Deserialize)]
struct UpdateContainerRequest {
    container_ref: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    name: Option<Option<String>>,
    root_path: Option<String>,
    /// `false` re-registers a row that was unregistered earlier.
    unregistered: Option<bool>,
}

#[derive(Deserialize)]
struct ListContainersQuery {
    /// Unregistered rows are hidden by default — they are history, and a stale
    /// ref resolves no better than a missing one.
    #[serde(default)]
    include_unregistered: bool,
}

/// Registers a container faber should know about. It does not create one: the
/// user owns container lifecycle, and this row is an assertion that faber knows
/// the ref, not that the ref resolves.
async fn create_container(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Json(input): Json<CreateContainerRequest>,
) -> ApiResult<(StatusCode, Json<ContainerResponse>)> {
    let container_ref = input.container_ref.trim();
    if container_ref.is_empty() {
        return Err(AppError::BadRequest("container_ref is required".into()));
    }

    let root_path = input.root_path.trim();
    if root_path.is_empty() {
        return Err(AppError::BadRequest("root_path is required".into()));
    }
    // Path normalization is mandatory: bind-mounted and native paths both
    // present as `root_path`, and a relative one would make the agent learn
    // host-specific path habits that silently fail to transfer.
    if !root_path.starts_with('/') {
        return Err(AppError::BadRequest("root_path must be absolute".into()));
    }

    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    if parent.exec_mode != ExecMode::Docker.as_str() {
        return Err(AppError::BadRequest(
            "containers can only be registered on a host whose exec_mode is 'docker'".into(),
        ));
    }

    let new_container = NewHostContainer {
        id: Uuid::now_v7(),
        host_id: parent.id,
        user_id: user.id,
        container_ref,
        name: trimmed(input.name.as_deref()),
        root_path,
        // Registration, not creation: the user made this container and faber
        // is only being told the ref.
        managed_at: None,
        image_id: None,
    };

    let inserted: HostContainer = diesel::insert_into(host_container::table)
        .values(&new_container)
        .returning(HostContainer::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::UniqueViolation,
                _,
            ) => AppError::BadRequest(format!(
                "'{container_ref}' is already registered on this host"
            )),
            other => AppError::db(other, "hosts.containers.create"),
        })?;

    Ok((StatusCode::CREATED, Json(container_response(&inserted))))
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// How long the daemon gets to create, start, or remove a container.
const DAEMON_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a pull gets. Generous, because a first pull of a large image over
/// a slow link is a normal thing rather than a stuck one — and bounded anyway,
/// because a request handler that waits forever holds a connection forever.
const PULL_TIMEOUT: Duration = Duration::from_secs(600);

/// What faber writes onto every container it creates, so the machine's owner
/// can tell where one came from without consulting faber's database.
const MANAGED_LABEL: &str = "dev.faber.managed";

/// The process a spawned container runs.
///
/// A container exists here to be exec'd into, so it has to stay up: the
/// image's own entrypoint usually returns, and a container whose entrypoint
/// returned is a stopped container with a confusing history. An image with no
/// `sleep` — a distroless or scratch base — needs `command` passed explicitly,
/// which is why this is a default and not a rule.
fn idle_command() -> Vec<String> {
    vec!["sleep".to_owned(), "infinity".to_owned()]
}

#[derive(Deserialize)]
struct SpawnContainerRequest {
    /// The template to create from. An image the caller owns, not a raw
    /// registry reference: the reference, the default root, and the default
    /// mounts are a set the user already assembled and named, and letting a
    /// spawn bypass it would make the template decorative.
    image_id: Uuid,
    /// The container's name on the machine, and its label in faber. Generated
    /// when omitted.
    name: Option<String>,
    /// Overrides the template's `default_root_path`.
    root_path: Option<String>,
    /// Overrides the template's `default_mounts` entirely — `[]` spawns with
    /// none, and omitting the field keeps the template's.
    mounts: Option<Vec<MountRequest>>,
    /// Environment for the container's own process. Not the agent's: what an
    /// exec sees is decided at bind, per target, and cannot be set from here.
    env: Option<BTreeMap<String, String>>,
    /// Overrides [`idle_command`].
    command: Option<Vec<String>>,
}

/// One bind mount. `source` is a path on the machine the daemon runs on, which
/// is the remote machine for an ssh host — not a path on the faber server.
#[derive(Deserialize, Serialize)]
struct MountRequest {
    source: String,
    target: String,
    #[serde(default)]
    read_only: bool,
}

impl From<&MountRequest> for engine::Mount {
    fn from(mount: &MountRequest) -> Self {
        engine::Mount {
            source: mount.source.clone(),
            target: mount.target.clone(),
            read_only: mount.read_only,
        }
    }
}

/// Docker accepts `[a-zA-Z0-9][a-zA-Z0-9_.-]*` and nothing else. Checked here
/// so a bad name is a 400 naming the rule rather than a daemon error relayed
/// through two layers.
fn validate_container_name(name: &str) -> Result<(), AppError> {
    let acceptable = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-';
    let starts_well = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    if !starts_well || !name.chars().all(acceptable) {
        return Err(AppError::BadRequest(
            "a container name must start with a letter or digit and contain only \
             letters, digits, '_', '.', and '-'"
                .into(),
        ));
    }
    Ok(())
}

/// Resolves a spawn request's template and root path.
///
/// The template has to be one of the caller's own: the reference, the default
/// root, and the default mounts are a set they already assembled and named,
/// and letting a spawn bypass it with a raw reference would make the template
/// decorative.
async fn resolve_template(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    input: &SpawnContainerRequest,
) -> Result<(Image, String), AppError> {
    let template: Image = image::table
        .filter(image::id.eq(input.image_id))
        .filter(image::user_id.eq(user_id))
        .select(Image::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "hosts.containers.spawn.load_image"))?
        .ok_or_else(|| AppError::BadRequest("no such image".to_owned()))?;

    let root_path = match trimmed(input.root_path.as_deref()) {
        Some(path) => path.to_owned(),
        None => template.default_root_path.clone(),
    };
    if !root_path.starts_with('/') {
        return Err(AppError::BadRequest("root_path must be absolute".into()));
    }

    // The root path is a bind mount destination, and the daemon refuses a few
    // of them. Refusing here is a better message than a raw docker error
    // relayed through two layers.
    if root_path.trim_end_matches('/').is_empty() {
        return Err(AppError::BadRequest(
            "root_path cannot be '/' — it is a mount destination, and the \
             daemon will not mount over a container's own root"
                .into(),
        ));
    }

    Ok((template, root_path))
}

/// Creates a container on the host and registers it as one faber manages.
///
/// The registration is written *after* the container exists, and the
/// container is removed again if the registration fails — because the
/// alternative is a container running on a user's machine that faber created
/// and has no record of, which nobody can attribute and nobody dares delete.
async fn spawn_container(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Json(input): Json<SpawnContainerRequest>,
) -> ApiResult<(StatusCode, Json<ContainerResponse>)> {
    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    let (template, root_path) = resolve_template(&mut conn, user.id, &input).await?;

    let mounts: Vec<MountRequest> = match input.mounts {
        Some(mounts) => mounts,
        // A template whose mounts do not parse is a configuration error the
        // user can see and fix, not a reason to silently spawn without them.
        None => match template.default_mounts.clone() {
            Some(value) => serde_json::from_value(value).map_err(|error| {
                AppError::BadRequest(format!(
                    "image '{}' has default_mounts faber cannot read: {error}",
                    template.name
                ))
            })?,
            None => Vec::new(),
        },
    };
    let engine_mounts: Vec<engine::Mount> = mounts.iter().map(engine::Mount::from).collect();

    let name = match trimmed(input.name.as_deref()) {
        Some(name) => name.to_owned(),
        // Short, prefixed, and unique: readable next to whatever else is
        // running on the machine, and unmistakably faber's.
        None => format!("faber-{}", Uuid::now_v7().simple()),
    };
    validate_container_name(&name)?;

    let command = match input.command {
        Some(command) if !command.is_empty() => command,
        _ => idle_command(),
    };
    let env: Vec<(String, String)> = input.env.unwrap_or_default().into_iter().collect();
    let labels = vec![
        (MANAGED_LABEL.to_owned(), "true".to_owned()),
        ("dev.faber.image".to_owned(), template.name.clone()),
    ];

    drop(conn);

    let daemon = crate::environments::reach_daemon(&state, user.id, &parent).await?;
    let create = engine::Create {
        name: Some(&name),
        image: &template.reference,
        cmd: &command,
        env: &env,
        working_dir: &root_path,
        mounts: &engine_mounts,
        labels: &labels,
    };

    let created =
        match with_timeout(DAEMON_TIMEOUT, engine::container_create(&daemon, &create)).await {
            Ok(id) => id,
            // The image is not on the machine yet. Pulling and retrying once is
            // the whole of it: a second failure is about the reference or the
            // registry, and retrying again would only take longer to say so.
            Err(Fault::Denied(Denial::NotFound { .. })) => {
                with_timeout(
                    PULL_TIMEOUT,
                    engine::image_pull(&daemon, &template.reference),
                )
                .await
                .map_err(crate::environments::fault)?;
                with_timeout(DAEMON_TIMEOUT, engine::container_create(&daemon, &create))
                    .await
                    .map_err(crate::environments::fault)?
            }
            Err(other) => return Err(crate::environments::fault(other)),
        };

    if let Err(error) =
        with_timeout(DAEMON_TIMEOUT, engine::container_start(&daemon, &created)).await
    {
        remove_created(&daemon, &created).await;
        return Err(crate::environments::fault(error));
    }

    // The id rather than the name: it is what the daemon resolves
    // unambiguously, and a name can be taken over by a container someone else
    // creates after this one is gone.
    let new_container = NewHostContainer {
        id: Uuid::now_v7(),
        host_id: parent.id,
        user_id: user.id,
        container_ref: &created,
        name: Some(&name),
        root_path: &root_path,
        managed_at: Some(Utc::now()),
        image_id: Some(template.id),
    };

    let mut conn = state.db.get().await?;
    let inserted: HostContainer = match diesel::insert_into(host_container::table)
        .values(&new_container)
        .returning(HostContainer::as_returning())
        .get_result(&mut conn)
        .await
    {
        Ok(row) => row,
        Err(error) => {
            remove_created(&daemon, &created).await;
            return Err(AppError::db(error, "hosts.containers.spawn.register"));
        }
    };

    Ok((StatusCode::CREATED, Json(container_response(&inserted))))
}

/// Undoes a half-finished spawn.
///
/// Best effort, and logged rather than reported: the caller is already being
/// told why the spawn failed, and "and the cleanup also failed" is operator
/// news. A container left behind here is one faber created and did not record,
/// which is exactly what the `dev.faber.managed` label is for.
async fn remove_created(daemon: &std::sync::Arc<dyn Daemon>, container: &str) {
    if let Err(error) =
        with_timeout(DAEMON_TIMEOUT, engine::container_remove(daemon, container)).await
    {
        tracing::error!(%container, %error, "could not remove a container faber had just created");
    }
}

/// Bounds a daemon call. A stalled socket must not hold a request handler
/// open, and the timeout is reported as unreachable because that is what it
/// is — nothing is known about whether the request was malformed.
async fn with_timeout<T>(
    limit: Duration,
    call: impl std::future::Future<Output = Result<T, Fault>>,
) -> Result<T, Fault> {
    match tokio::time::timeout(limit, call).await {
        Ok(result) => result,
        Err(_) => Err(Fault::Unreachable(format!(
            "the docker daemon did not answer within {limit:?}"
        ))),
    }
}

async fn list_containers(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Query(query): Query<ListContainersQuery>,
) -> ApiResult<Json<Vec<ContainerResponse>>> {
    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    let mut statement = host_container::table
        .filter(host_container::host_id.eq(parent.id))
        .filter(host_container::user_id.eq(user.id))
        .into_boxed();

    if !query.include_unregistered {
        statement = statement.filter(host_container::unregistered_at.is_null());
    }

    let rows: Vec<HostContainer> = statement
        .order(host_container::created_at.asc())
        .select(HostContainer::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.containers.list"))?;

    Ok(Json(rows.iter().map(container_response).collect()))
}

/// Loads a container the caller owns, or `NotFound`.
///
/// Authorized on the container rather than through its host: the container
/// row is what knows whose the container is.
async fn owned_container(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    id: Uuid,
) -> Result<HostContainer, AppError> {
    let row: Option<HostContainer> = host_container::table
        .filter(host_container::id.eq(id))
        .filter(host_container::user_id.eq(user_id))
        .select(HostContainer::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "hosts.containers.load"))?;

    row.ok_or(AppError::NotFound)
}

async fn update_container(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateContainerRequest>,
) -> ApiResult<Json<ContainerResponse>> {
    if let Some(ref container_ref) = input.container_ref
        && container_ref.trim().is_empty()
    {
        return Err(AppError::BadRequest("container_ref cannot be empty".into()));
    }

    if let Some(ref root_path) = input.root_path {
        let root_path = root_path.trim();
        if root_path.is_empty() {
            return Err(AppError::BadRequest("root_path cannot be empty".into()));
        }
        if !root_path.starts_with('/') {
            return Err(AppError::BadRequest("root_path must be absolute".into()));
        }
    }

    let mut conn = state.db.get().await?;
    let current = owned_container(&mut conn, user.id, id).await?;

    let patch = UpdateHostContainer {
        container_ref: input.container_ref.as_deref().map(str::trim),
        name: input.name.as_ref().map(|v| trimmed(v.as_deref())),
        root_path: input.root_path.as_deref().map(str::trim),
        unregistered_at: input.unregistered.map(|u| u.then(Utc::now)),
    };

    let updated: HostContainer =
        diesel::update(host_container::table.filter(host_container::id.eq(current.id)))
            .set(patch)
            .returning(HostContainer::as_returning())
            .get_result(&mut conn)
            .await
            .map_err(|err| match err {
                diesel::result::Error::NotFound => AppError::NotFound,
                diesel::result::Error::DatabaseError(
                    diesel::result::DatabaseErrorKind::UniqueViolation,
                    _,
                ) => AppError::BadRequest("that ref is already registered on this host".into()),
                other => AppError::db(other, "hosts.containers.update"),
            })?;

    Ok(Json(container_response(&updated)))
}

#[derive(Deserialize)]
struct UnregisterQuery {
    /// Also destroy the container. Permitted only for a container faber
    /// created, and never the default: `DELETE` on a registration has always
    /// meant "stop knowing about this", and quietly widening that to "and
    /// destroy it" would destroy containers on the strength of an old client's
    /// habits.
    #[serde(default)]
    destroy: bool,
}

/// Ends the registration, and — only when asked, and only for a container
/// faber created — destroys the container too.
///
/// A container faber merely registered is never touched: faber did not make it
/// and does not know what else it is for. `PATCH { unregistered: false }`
/// brings the row back, which a destroyed container obviously cannot honour,
/// so the row stays tombstoned either way and the response says which happened
/// by whether the container still exists.
async fn unregister_container(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<UnregisterQuery>,
) -> ApiResult<StatusCode> {
    let mut conn = state.db.get().await?;
    let current = owned_container(&mut conn, user.id, id).await?;

    if query.destroy {
        if !current.managed() {
            return Err(AppError::BadRequest(
                "faber did not create this container, so it will not destroy it; \
                 unregister it without `destroy` and remove it yourself"
                    .into(),
            ));
        }

        // The container is already known to be the caller's; the host load
        // is only for reaching its daemon.
        let parent = owned_host(&mut conn, user.id, current.host_id).await?;
        drop(conn);

        // Destroyed first: a registration removed before the container leaves
        // a container nobody has a record of, and this order fails the other
        // way — a container removed whose row is still there is visible, and
        // the next call reports it gone.
        let daemon = crate::environments::reach_daemon(&state, user.id, &parent).await?;
        with_timeout(
            DAEMON_TIMEOUT,
            engine::container_remove(&daemon, &current.container_ref),
        )
        .await
        .map_err(crate::environments::fault)?;

        conn = state.db.get().await?;
    }

    diesel::update(host_container::table.filter(host_container::id.eq(current.id)))
        .set(host_container::unregistered_at.eq(Some(Utc::now())))
        .execute(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.containers.unregister"))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RecordProbeRequest {
    /// Scopes the observation to one registered container on this host.
    container_id: Option<Uuid>,
    ok: bool,
    error: Option<String>,
    os: Option<String>,
    arch: Option<String>,
    shell: Option<String>,
    /// Capability manifest, e.g. `{"git": "2.43.0"}`. Capability is discovered
    /// by probe, never assumed from the host's mode.
    tools: Option<Value>,
    root_path: Option<String>,
}

#[derive(Deserialize)]
struct ListProbesQuery {
    limit: Option<i64>,
}

/// Appends one observation. Write-only history: there is no route to amend or
/// delete a probe, because the log is what makes "last attempt: connection
/// refused" a fact rather than a cached status.
async fn record_probe(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Json(input): Json<RecordProbeRequest>,
) -> ApiResult<(StatusCode, Json<ProbeResponse>)> {
    if !input.ok && trimmed(input.error.as_deref()).is_none() {
        return Err(AppError::BadRequest(
            "error is required when ok is false".into(),
        ));
    }

    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    if let Some(container_id) = input.container_id {
        let container = owned_container(&mut conn, user.id, container_id).await?;
        if container.host_id != parent.id {
            return Err(AppError::BadRequest(
                "container_id names a container on a different host".into(),
            ));
        }
    }

    let new_probe = NewHostProbe {
        id: Uuid::now_v7(),
        host_id: parent.id,
        container_id: input.container_id,
        ok: input.ok,
        error: trimmed(input.error.as_deref()),
        os: trimmed(input.os.as_deref()),
        arch: trimmed(input.arch.as_deref()),
        shell: trimmed(input.shell.as_deref()),
        tools: input.tools,
        root_path: trimmed(input.root_path.as_deref()),
    };

    let inserted: HostProbe = diesel::insert_into(host_probe::table)
        .values(&new_probe)
        .returning(HostProbe::as_returning())
        .get_result(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.probes.create"))?;

    Ok((StatusCode::CREATED, Json(probe_response(&inserted))))
}

async fn list_probes(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(host_id): Path<Uuid>,
    Query(query): Query<ListProbesQuery>,
) -> ApiResult<Json<Vec<ProbeResponse>>> {
    let mut conn = state.db.get().await?;
    let parent = owned_host(&mut conn, user.id, host_id).await?;

    let rows: Vec<HostProbe> = host_probe::table
        .filter(host_probe::host_id.eq(parent.id))
        .order(host_probe::probed_at.desc())
        .limit(clamp_limit(query.limit))
        .select(HostProbe::as_select())
        .load(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "hosts.probes.list"))?;

    Ok(Json(rows.iter().map(probe_response).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_host_may_not_carry_ssh_configuration() {
        // Mirrors the DB checks, so a mismatch comes back naming the field
        // rather than as an opaque constraint violation.
        assert!(validate_transport_config(Transport::Local, None, None, true).is_ok());
        assert!(validate_transport_config(Transport::Local, Some("a@b:22"), None, true).is_err());
        assert!(validate_transport_config(Transport::Local, None, Some("SHA256:x"), true).is_err());
    }

    #[test]
    fn local_hosts_can_be_disabled_by_configuration() {
        assert!(validate_transport_config(Transport::Local, None, None, false).is_err());
        assert!(validate_transport_config(Transport::Ssh, Some("a@b:22"), None, false).is_ok());
    }

    #[test]
    fn an_agent_host_may_not_carry_ssh_configuration_either() {
        // Same rule as local, and for a related reason: the address and
        // fingerprint an agent host would carry live in `agent_credential`
        // instead, keyed by host_id rather than sitting on this row.
        assert!(validate_transport_config(Transport::Agent, None, None, true).is_ok());
        assert!(validate_transport_config(Transport::Agent, Some("a@b:22"), None, true).is_err());
        assert!(validate_transport_config(Transport::Agent, None, Some("SHA256:x"), true).is_err());
    }

    #[test]
    fn an_ssh_host_needs_an_address_but_not_yet_a_host_key() {
        assert!(validate_transport_config(Transport::Ssh, None, None, true).is_err());
        // No host key is the normal state before first contact: the connection
        // records what it saw, and everything after verifies against it.
        assert!(validate_transport_config(Transport::Ssh, Some("a@b:22"), None, true).is_ok());
        assert!(
            validate_transport_config(Transport::Ssh, Some("a@b:22"), Some("SHA256:x"), true)
                .is_ok()
        );
    }

    #[test]
    fn a_container_name_is_checked_here_rather_than_relayed_from_the_daemon() {
        assert!(validate_container_name("faber-01920b").is_ok());
        assert!(validate_container_name("build.1_x-2").is_ok());
        // The daemon's own rule: the first character carries no punctuation.
        assert!(validate_container_name("-leading-dash").is_err());
        assert!(validate_container_name("has space").is_err());
        assert!(validate_container_name("").is_err());
    }

    #[test]
    fn a_blank_field_is_not_a_value() {
        // Whitespace reaching a nullable column would make "unset" and "set to
        // nothing" two states that look different and behave the same.
        assert_eq!(trimmed(Some("  ")), None);
        assert_eq!(trimmed(Some(" SHA256:x ")), Some("SHA256:x"));
        assert!(validate_transport_config(Transport::Local, Some(""), Some(""), true).is_ok());
    }
}
