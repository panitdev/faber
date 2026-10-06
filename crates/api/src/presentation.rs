//! Dormant live-server presentation core.
//!
//! Presentation URLs are unguessable capability URLs: possession grants
//! access. This subsystem prevents practical enumeration and unintended
//! discovery, but does not try to preserve URL secrecy after disclosure or a
//! database-only compromise. A confidential service must authenticate its own
//! users before it is presented.
//!
//! Nothing in this module is mounted in the application router. Eventual
//! activation should dispatch the configured preview host here before the
//! existing API router, without adding a listing endpoint.

use chrono::Utc;
use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, RunQueryDsl};
use rand::RngCore;
use uuid::Uuid;

use crate::{
    access::authorize_session,
    error::{ApiResult, AppError},
    models::{
        presentation::{NewPresentation, Presentation, UpstreamHostMode},
        session::SessionEnvironment,
    },
    schema::{presentation, session_environment},
    state::AppState,
};

pub mod proxy;
pub mod resolve;
pub mod tool;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session_id: Uuid,
    pub environment_label: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub upstream_host_mode: UpstreamHostMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presented {
    pub generation_id: Uuid,
    pub url: String,
}

pub enum TokenResolution {
    Active {
        presentation: Presentation,
        binding: SessionEnvironment,
    },
    Gone,
    Unknown,
}

/// Atomically gets or creates the active generation for a target.
pub async fn present(
    state: &AppState,
    actor: Uuid,
    target: &Target,
    options: Options,
) -> ApiResult<Presented> {
    let mut conn = state.db.get().await?;
    let target = target.clone();
    let row = conn
        .transaction::<_, AppError, _>(move |conn| {
            async move { present_row(conn, actor, &target, options).await }.scope_boxed()
        })
        .await?;

    Ok(Presented {
        generation_id: row.id,
        url: presentation_url(state, &row.token),
    })
}

/// Tombstones the active generation addressed by the same target used to
/// create it. The token stays on the row so a known revoked URL is a 410.
pub async fn revoke(state: &AppState, actor: Uuid, target: &Target) -> ApiResult<bool> {
    let mut conn = state.db.get().await?;
    let target = target.clone();
    let updated = conn
        .transaction::<_, AppError, _>(move |conn| {
            async move {
                authorize_session(conn, actor, target.session_id).await?;
                lock_target(conn, &target).await?;
                revoke_row(conn, &target).await
            }
            .scope_boxed()
        })
        .await?;
    Ok(updated > 0)
}

/// Direct indexed lookup by the plaintext capability token.
pub async fn resolve_token(state: &AppState, token: &str) -> ApiResult<TokenResolution> {
    if !valid_token(token) {
        return Ok(TokenResolution::Unknown);
    }
    let mut conn = state.db.get().await?;
    resolve_token_on(&mut conn, token).await
}

async fn resolve_token_on(
    conn: &mut diesel_async::AsyncPgConnection,
    token: &str,
) -> ApiResult<TokenResolution> {
    let row: Option<Presentation> = presentation::table
        .filter(presentation::token.eq(token))
        .select(Presentation::as_select())
        .first(&mut *conn)
        .await
        .optional()
        .map_err(|error| AppError::db(error, "presentation.resolve.token"))?;
    let Some(row) = row else {
        return Ok(TokenResolution::Unknown);
    };
    if row.revoked_at.is_some() {
        return Ok(TokenResolution::Gone);
    }
    let binding: Option<SessionEnvironment> = session_environment::table
        .filter(session_environment::session_id.eq(row.session_id))
        .filter(session_environment::label.eq(&row.environment_label))
        .select(SessionEnvironment::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|error| AppError::db(error, "presentation.resolve.binding"))?;
    match binding {
        Some(binding) if binding.removed_at.is_none() => Ok(TokenResolution::Active {
            presentation: row,
            binding,
        }),
        _ => Ok(TokenResolution::Gone),
    }
}

async fn present_row(
    conn: &mut diesel_async::AsyncPgConnection,
    actor: Uuid,
    target: &Target,
    options: Options,
) -> ApiResult<Presentation> {
    authorize_session(conn, actor, target.session_id).await?;
    let binding: SessionEnvironment = session_environment::table
        .filter(session_environment::session_id.eq(target.session_id))
        .filter(session_environment::label.eq(&target.environment_label))
        .filter(session_environment::removed_at.is_null())
        .select(SessionEnvironment::as_select())
        .first(&mut *conn)
        .await
        .optional()
        .map_err(|error| AppError::db(error, "presentation.present.binding"))?
        .ok_or_else(|| AppError::BadRequest("this environment is not bound".into()))?;

    // One transaction per target may decide whether an active row exists.
    lock_target(conn, target).await?;
    if let Some(active) = active_for_target(conn, target).await? {
        return Ok(active);
    }

    // The global uniqueness rule includes revoked generations. A collision
    // mints again instead of ever recycling the old token.
    for _ in 0..4 {
        let token = mint_token();
        let inserted = diesel::insert_into(presentation::table)
            .values(NewPresentation {
                id: Uuid::now_v7(),
                session_id: binding.session_id,
                environment_label: &binding.label,
                port: i32::from(target.port),
                token: &token,
                upstream_host_mode: options.upstream_host_mode.as_str(),
            })
            .on_conflict(presentation::token)
            .do_nothing()
            .returning(Presentation::as_returning())
            .get_result(&mut *conn)
            .await
            .optional()
            .map_err(|error| AppError::db(error, "presentation.present.insert"))?;
        if let Some(inserted) = inserted {
            return Ok(inserted);
        }
    }
    tracing::error!("four cryptographic presentation-token collisions");
    Err(AppError::Internal)
}

async fn revoke_row(
    conn: &mut diesel_async::AsyncPgConnection,
    target: &Target,
) -> ApiResult<usize> {
    diesel::update(
        presentation::table
            .filter(presentation::session_id.eq(target.session_id))
            .filter(presentation::environment_label.eq(&target.environment_label))
            .filter(presentation::port.eq(i32::from(target.port)))
            .filter(presentation::revoked_at.is_null()),
    )
    .set(presentation::revoked_at.eq(Some(Utc::now())))
    .execute(conn)
    .await
    .map_err(|error| AppError::db(error, "presentation.revoke"))
}

async fn active_for_target(
    conn: &mut diesel_async::AsyncPgConnection,
    target: &Target,
) -> ApiResult<Option<Presentation>> {
    presentation::table
        .filter(presentation::session_id.eq(target.session_id))
        .filter(presentation::environment_label.eq(&target.environment_label))
        .filter(presentation::port.eq(i32::from(target.port)))
        .filter(presentation::revoked_at.is_null())
        .select(Presentation::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|error| AppError::db(error, "presentation.present.active"))
}

async fn lock_target(conn: &mut diesel_async::AsyncPgConnection, target: &Target) -> ApiResult<()> {
    // UUID and decimal port have fixed grammars, so the label can safely
    // occupy the remainder without a sentinel PostgreSQL text cannot carry.
    let lock_key = format!(
        "{}:{}:{}",
        target.session_id, target.port, target.environment_label
    );
    diesel::sql_query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind::<diesel::sql_types::Text, _>(&lock_key)
        .execute(conn)
        .await
        .map(|_| ())
        .map_err(|error| AppError::db(error, "presentation.target.lock"))
}

fn mint_token() -> String {
    // Keep `p-` plus the encoded token below DNS's 63-character label limit.
    let mut bytes = [0_u8; 30];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    // Hostnames are case-insensitive, so the token must not contain case.
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn valid_token(token: &str) -> bool {
    token.len() == 60
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn presentation_url(state: &AppState, token: &str) -> String {
    let port = (state.config.preview_domain == "localhost")
        .then(|| format!(":{}", state.config.api_port))
        .unwrap_or_default();
    format!(
        "{}://p-{}.{}{}",
        state.config.preview_scheme, token, state.config.preview_domain, port
    )
}

/// Extracts a token only from the exact configured preview hostname shape.
pub fn token_from_host<'a>(host: &'a str, domain: &str) -> Option<&'a str> {
    let host = host.split(':').next()?;
    let token = host
        .strip_prefix("p-")?
        .strip_suffix(&format!(".{domain}"))?;
    (!token.contains('.') && valid_token(token)).then_some(token)
}

/// Whether `host` belongs to the preview wildcard. This intentionally does
/// not validate the token: unknown and malformed capability hosts must reach
/// the presentation handler and receive the same 404 rather than falling
/// through to an API route.
pub fn is_preview_host(host: &str, domain: &str) -> bool {
    let host = host.split(':').next().unwrap_or_default();
    let suffix = format!(".{domain}");
    host.len() > "p-".len() + suffix.len()
        && host
            .get(.."p-".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("p-"))
        && host.ends_with(&suffix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel_async::{AsyncConnection, AsyncPgConnection};

    async fn connection() -> Option<AsyncPgConnection> {
        let url = std::env::var("DATABASE_URL").ok()?;
        Some(
            AsyncPgConnection::establish(&url)
                .await
                .expect("DATABASE_URL is set but not connectable"),
        )
    }

    async fn setup_target(conn: &mut AsyncPgConnection) -> (Uuid, Target) {
        let actor = Uuid::now_v7();
        diesel::sql_query("INSERT INTO users (id, identity_id) VALUES ($1, $2)")
            .bind::<diesel::sql_types::Uuid, _>(actor)
            .bind::<diesel::sql_types::Uuid, _>(Uuid::now_v7())
            .execute(&mut *conn)
            .await
            .unwrap();
        let workspace = crate::access::personal_workspace(conn, actor)
            .await
            .unwrap();
        let session_id = Uuid::now_v7();
        diesel::insert_into(crate::schema::session::table)
            .values(crate::models::session::NewSession {
                id: session_id,
                workspace_id: workspace.id,
                title: None,
                created_at: crate::models::now_epoch(),
                project_id: None,
            })
            .execute(&mut *conn)
            .await
            .unwrap();
        let host_id = Uuid::now_v7();
        diesel::sql_query(
            "INSERT INTO host (id, user_id, name, transport, exec_mode, root_path) \
             VALUES ($1, $2, $3, 'local', 'direct', '/tmp')",
        )
        .bind::<diesel::sql_types::Uuid, _>(host_id)
        .bind::<diesel::sql_types::Uuid, _>(actor)
        .bind::<diesel::sql_types::Text, _>(format!("preview-{host_id}"))
        .execute(&mut *conn)
        .await
        .unwrap();
        diesel::insert_into(session_environment::table)
            .values(crate::models::session::NewSessionEnvironment {
                session_id,
                label: "dev",
                host_id,
                container_id: None,
                added_at: crate::models::now_epoch(),
            })
            .execute(conn)
            .await
            .unwrap();
        (
            actor,
            Target {
                session_id,
                environment_label: "dev".into(),
                port: 5173,
            },
        )
    }

    #[test]
    fn tokens_are_dns_safe_and_not_reused_in_a_sample() {
        let mut found = std::collections::HashSet::new();
        for _ in 0..1_000 {
            let token = mint_token();
            assert!(valid_token(&token));
            assert_eq!(token.len(), 60);
            assert!(
                token
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            );
            assert!(found.insert(token));
        }
    }

    #[test]
    fn host_parser_accepts_only_the_exact_capability_host() {
        let token = mint_token();
        assert_eq!(
            token_from_host(&format!("p-{token}.preview.test"), "preview.test"),
            Some(token.as_str())
        );
        assert_eq!(
            token_from_host(&format!("p-{token}.preview.test:3001"), "preview.test"),
            Some(token.as_str())
        );
        assert_eq!(
            token_from_host(
                &format!("p-{}.preview.test", token.to_uppercase()),
                "preview.test"
            ),
            None
        );
        assert_eq!(token_from_host("preview.test", "preview.test"), None);
        assert_eq!(
            token_from_host("p-short.preview.test", "preview.test"),
            None
        );
    }

    #[test]
    fn preview_host_matcher_keeps_malformed_capabilities_on_the_preview_path() {
        assert!(is_preview_host("p-short.preview.test", "preview.test"));
        assert!(is_preview_host("p-token.preview.test:3001", "preview.test"));
        assert!(!is_preview_host("preview.test", "preview.test"));
        assert!(!is_preview_host("other.preview.test", "preview.test"));
    }

    #[tokio::test]
    async fn lifecycle_is_idempotent_resolvable_and_never_reuses_a_token() {
        let Some(mut conn) = connection().await else {
            return;
        };
        conn.begin_test_transaction().await.unwrap();
        let (actor, target) = setup_target(&mut conn).await;

        let first = present_row(&mut conn, actor, &target, Options::default())
            .await
            .unwrap();
        let repeated = present_row(&mut conn, actor, &target, Options::default())
            .await
            .unwrap();
        assert_eq!(repeated.id, first.id);
        assert_eq!(repeated.token, first.token);
        assert!(matches!(
            resolve_token_on(&mut conn, &first.token).await.unwrap(),
            TokenResolution::Active { .. }
        ));

        let outsider = Uuid::now_v7();
        diesel::sql_query("INSERT INTO users (id, identity_id) VALUES ($1, $2)")
            .bind::<diesel::sql_types::Uuid, _>(outsider)
            .bind::<diesel::sql_types::Uuid, _>(Uuid::now_v7())
            .execute(&mut conn)
            .await
            .unwrap();
        assert!(matches!(
            present_row(&mut conn, outsider, &target, Options::default()).await,
            Err(AppError::NotFound)
        ));

        assert_eq!(revoke_row(&mut conn, &target).await.unwrap(), 1);
        assert!(matches!(
            resolve_token_on(&mut conn, &first.token).await.unwrap(),
            TokenResolution::Gone
        ));
        assert!(matches!(
            resolve_token_on(&mut conn, &mint_token()).await.unwrap(),
            TokenResolution::Unknown
        ));

        let next = present_row(&mut conn, actor, &target, Options::default())
            .await
            .unwrap();
        assert_ne!(next.id, first.id);
        assert_ne!(next.token, first.token);

        diesel::update(
            session_environment::table
                .filter(session_environment::session_id.eq(target.session_id))
                .filter(session_environment::label.eq(&target.environment_label)),
        )
        .set(session_environment::removed_at.eq(Some(crate::models::now_epoch())))
        .execute(&mut conn)
        .await
        .unwrap();
        assert!(matches!(
            resolve_token_on(&mut conn, &next.token).await.unwrap(),
            TokenResolution::Gone
        ));
    }

    #[tokio::test]
    async fn concurrent_creation_returns_one_active_generation() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let mut setup = AsyncPgConnection::establish(&url).await.unwrap();
        let (actor, target) = setup_target(&mut setup).await;

        let create = |target: Target| {
            let url = url.clone();
            tokio::spawn(async move {
                let mut conn = AsyncPgConnection::establish(&url).await.unwrap();
                conn.transaction::<_, AppError, _>(move |conn| {
                    async move { present_row(conn, actor, &target, Options::default()).await }
                        .scope_boxed()
                })
                .await
                .unwrap()
            })
        };
        let (left, right) = tokio::join!(create(target.clone()), create(target));
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(left.id, right.id);
        assert_eq!(left.token, right.token);

        diesel::sql_query("DELETE FROM users WHERE id = $1")
            .bind::<diesel::sql_types::Uuid, _>(actor)
            .execute(&mut setup)
            .await
            .unwrap();
    }
}
