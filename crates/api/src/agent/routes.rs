//! Enrollment and connection for agent-transport hosts.
//!
//! Three endpoints, three different callers:
//!
//! - `POST /api/hosts/{id}/agent/enroll` — the operator, authenticated as a
//!   user, asking for a one-time install command.
//! - `GET /api/hosts/{id}/agent` — the same operator, watching for the
//!   daemon they just installed to show up.
//! - `POST /api/agent/enroll` — the daemon's first run, presenting the
//!   bootstrap token from that command and getting back the long-lived
//!   credential it will hold from then on.
//! - `GET /api/agent/connect` — every run after that: the daemon dials out,
//!   presents the long-lived credential, and the connection is upgraded and
//!   handed to [`SshSession::from_stream`].

use axum::{
    Json, Router,
    extract::{
        Path, State,
        ws::{WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    routing::{get, post},
};
use base64::Engine as _;
use chrono::{Duration as ChronoDuration, Utc};
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use environment::ssh::{HostKey, SshSession, fingerprint_of};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{auth::AgentIdentity, auth::hash_token, link::WsStream};
use crate::{
    auth::AuthUser,
    error::{ApiResult, AppError},
    models::{
        agent::{AgentCredential, NewAgentCredential, NewAgentEnrollment},
        host::{Host, Transport},
    },
    schema::{agent_credential, agent_enrollment, host},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/hosts/{id}/agent/enroll", post(enroll))
        .route("/api/hosts/{id}/agent", get(status))
        .route("/api/agent/enroll", post(exchange))
        .route("/api/agent/connect", get(connect))
        .merge(super::install::router())
}

/// How long a bootstrap token stays redeemable. Long enough to copy a
/// command onto a machine and run it, short enough that a token nobody used
/// is not a standing credential.
const ENROLLMENT_TTL: ChronoDuration = ChronoDuration::hours(1);

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Operator: issue a bootstrap token
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct EnrollResponse {
    /// One-time; never stored or shown again after the daemon exchanges it.
    pub token: String,
    pub expires_at: chrono::DateTime<Utc>,
    /// A copy-pasteable one-liner, mirroring the pattern of Cloudflare
    /// Tunnel or Tailscale: it fetches the installer script from this API,
    /// which downloads the daemon binary this API serves and hands it the
    /// token below.
    ///
    /// The token appears in the target's shell history and, briefly, its
    /// process list. That is the accepted cost of the pattern — the same one
    /// Tailscale and Cloudflare take — and it is bounded by the token being
    /// single-use and valid for an hour.
    pub install_command: String,
}

/// The caller's own agent host, or the error that says why it isn't one.
///
/// Both operator-facing routes want exactly this, and want it to fail the
/// same way: a host somebody else owns is indistinguishable from one that
/// does not exist, and a host of another transport has nothing to enroll or
/// report on.
async fn owned_agent_host(
    conn: &mut diesel_async::AsyncPgConnection,
    user_id: Uuid,
    id: Uuid,
    context: &'static str,
) -> Result<Host, AppError> {
    let target: Host = host::table
        .filter(host::id.eq(id))
        .filter(host::user_id.eq(user_id))
        .select(Host::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, context))?
        .ok_or(AppError::NotFound)?;

    if target.transport != Transport::Agent.as_str() {
        return Err(AppError::BadRequest(
            "this host's transport is not 'agent'".into(),
        ));
    }

    Ok(target)
}

async fn enroll(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<EnrollResponse>)> {
    let mut conn = state.db.get().await?;
    let target = owned_agent_host(&mut conn, user.id, id, "agent.enroll.load_host").await?;
    let issued = issue_enrollment(&state, &mut conn, target.id).await?;
    Ok((StatusCode::CREATED, Json(issued)))
}

/// Issues a bootstrap token for a host and builds the command that redeems
/// it.
///
/// The daemon installs as a `systemctl --user` unit under the account it is
/// enrolled from: every host is one a user owns, so user scope is the only
/// scope there is.
pub async fn issue_enrollment(
    state: &AppState,
    conn: &mut diesel_async::AsyncPgConnection,
    host_id: Uuid,
) -> ApiResult<EnrollResponse> {
    // Before the token is issued, not after: an install command faber cannot
    // build is a token that would sit there redeemable with no way to
    // redeem it, and superseding the previous enrollment below would have
    // already invalidated whatever the operator was holding.
    let base = super::install::public_url(state)?;

    let token = generate_token();
    let expires_at = Utc::now() + ENROLLMENT_TTL;

    // A second call to this endpoint is a re-issue, not an addition: an
    // operator who lost the first command or wants a fresh window should not
    // leave the old token sitting around redeemable in parallel. Expiring it
    // rather than marking it consumed keeps `consumed_at` meaning what it
    // says — this token was never exchanged, it was superseded.
    diesel::update(
        agent_enrollment::table
            .filter(agent_enrollment::host_id.eq(host_id))
            .filter(agent_enrollment::consumed_at.is_null()),
    )
    .set(agent_enrollment::expires_at.eq(Utc::now()))
    .execute(conn)
    .await
    .map_err(|err| AppError::db(err, "agent.enroll.supersede"))?;

    diesel::insert_into(agent_enrollment::table)
        .values(&NewAgentEnrollment {
            id: Uuid::now_v7(),
            host_id,
            token_hash: &hash_token(&token),
            expires_at,
        })
        .execute(conn)
        .await
        .map_err(|err| AppError::db(err, "agent.enroll.insert"))?;

    let install_command =
        format!("curl -fsSL {base}/api/agent/install.sh | sh -s -- --token {token}");

    Ok(EnrollResponse {
        install_command,
        token,
        expires_at,
    })
}

/// When a host's daemon last exchanged a bootstrap token for the credential
/// it reconnects with, if it ever has.
pub async fn enrolled_at(
    conn: &mut diesel_async::AsyncPgConnection,
    host_id: Uuid,
) -> ApiResult<Option<chrono::DateTime<Utc>>> {
    agent_credential::table
        .filter(agent_credential::host_id.eq(host_id))
        .filter(agent_credential::revoked_at.is_null())
        .select(agent_credential::issued_at)
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "agent.enrolled_at"))
}

// ---------------------------------------------------------------------------
// Operator: is the daemon there yet?
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct StatusResponse {
    /// Whether a daemon is on the wire *right now*, read from the live
    /// connection registry rather than from a column. Nothing stores this:
    /// a connection that died is gone from the registry, so there is no
    /// stale value for a caller to read.
    connected: bool,
    /// When the daemon exchanged its bootstrap token for the credential it
    /// reconnects with, or `null` if that has not happened. This is what
    /// separates "nobody has run the install command" from "the daemon is
    /// installed and dialing".
    enrolled_at: Option<chrono::DateTime<Utc>>,
}

async fn status(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StatusResponse>> {
    let mut conn = state.db.get().await?;
    let target = owned_agent_host(&mut conn, user.id, id, "agent.status.load_host").await?;

    Ok(Json(StatusResponse {
        connected: state.agents.get(target.id).is_some(),
        enrolled_at: enrolled_at(&mut conn, target.id).await?,
    }))
}

// ---------------------------------------------------------------------------
// Daemon: exchange the bootstrap token for a long-lived credential
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ExchangeRequest {
    token: String,
    /// The daemon's freshly generated SSH host public key, OpenSSH format.
    /// Pinned from here on — there is no TOFU window for agent hosts,
    /// because this exchange is itself the one-time authenticated moment
    /// TOFU exists to approximate.
    host_pubkey: String,
}

#[derive(Serialize)]
struct ExchangeResponse {
    /// The long-lived credential. Shown exactly once — the daemon stores it
    /// and presents it on every reconnect; faber never sees the plaintext
    /// again after this response.
    credential: String,
}

async fn exchange(
    State(state): State<AppState>,
    Json(input): Json<ExchangeRequest>,
) -> ApiResult<(StatusCode, Json<ExchangeResponse>)> {
    if fingerprint_of(&input.host_pubkey).is_err() {
        return Err(AppError::BadRequest(
            "host_pubkey is not a readable OpenSSH public key".into(),
        ));
    }

    let hash = hash_token(&input.token);
    let mut conn = state.db.get().await?;

    let host_id: Uuid = diesel::update(
        agent_enrollment::table
            .filter(agent_enrollment::token_hash.eq(&hash))
            .filter(agent_enrollment::consumed_at.is_null())
            .filter(agent_enrollment::expires_at.gt(Utc::now())),
    )
    .set(agent_enrollment::consumed_at.eq(Some(Utc::now())))
    .returning(agent_enrollment::host_id)
    .get_result(&mut conn)
    .await
    .optional()
    .map_err(|err| AppError::db(err, "agent.exchange.consume"))?
    .ok_or_else(|| AppError::Unauthorized("unknown or expired enrollment token".into()))?;

    let credential = generate_token();

    // "Regenerate token" reaches the same state by the same path: a
    // new row, the old one revoked rather than overwritten, so the log keeps
    // what was there rather than losing it to an UPDATE. Enrollment can run
    // again on a host that already has an active credential — a daemon
    // reinstalled from scratch has no old credential to present — so the
    // previous row is revoked here rather than assumed absent.
    diesel::update(
        agent_credential::table
            .filter(agent_credential::host_id.eq(host_id))
            .filter(agent_credential::revoked_at.is_null()),
    )
    .set(agent_credential::revoked_at.eq(Some(Utc::now())))
    .execute(&mut conn)
    .await
    .map_err(|err| AppError::db(err, "agent.exchange.revoke_previous"))?;

    diesel::insert_into(agent_credential::table)
        .values(&NewAgentCredential {
            id: Uuid::now_v7(),
            host_id,
            token_hash: &hash_token(&credential),
            host_pubkey: &input.host_pubkey,
        })
        .execute(&mut conn)
        .await
        .map_err(|err| AppError::db(err, "agent.exchange.insert"))?;

    state.agents.evict(host_id).await;

    Ok((StatusCode::CREATED, Json(ExchangeResponse { credential })))
}

// ---------------------------------------------------------------------------
// Daemon: the live connection
// ---------------------------------------------------------------------------

async fn connect(
    State(state): State<AppState>,
    identity: AgentIdentity,
    ws: WebSocketUpgrade,
) -> impl axum::response::IntoResponse {
    ws.on_upgrade(move |socket| handle_connection(state, identity.credential, socket))
}

async fn handle_connection(state: AppState, credential: AgentCredential, socket: WebSocket) {
    let host_id = credential.host_id;
    let name = format!("agent:{host_id}");

    let expected = match fingerprint_of(&credential.host_pubkey) {
        Ok(fingerprint) => HostKey::Verify(fingerprint),
        Err(error) => {
            tracing::warn!(%host_id, %error, "agent host key on file is unreadable");
            return;
        }
    };

    let stream = WsStream::new(socket);
    match SshSession::from_stream(stream, &name, expected).await {
        Ok((session, _fingerprint)) => {
            state.agents.adopt(host_id, session).await;
            tracing::info!(%host_id, "agent connected");
        }
        Err(error) => {
            tracing::warn!(%host_id, %error, "agent connection rejected");
        }
    }
}
