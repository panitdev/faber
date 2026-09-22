//! Authorization walks for the conversation model.
//!
//! Reachability is always the same question — is the caller a member of the workspace
//! this row ultimately hangs off? — asked from a different depth: `workspace_member →
//! workspace → session → thread → run`. Each helper answers it with one join and returns
//! the loaded row so the handler does not query twice.
//!
//! A row the caller cannot reach is reported as [`AppError::NotFound`], never as a
//! forbidden: whether a session exists is itself information.

use diesel::{ExpressionMethods, JoinOnDsl, QueryDsl, SelectableHelper};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::{
    error::{ApiResult, AppError},
    models::{
        exchange::Exchange, run::Run, session::Session, thread::Thread, workspace::Workspace,
    },
    schema::{exchange, run, session, thread, workspace, workspace_member},
};

/// Maps a missing row onto `NotFound` while leaving real failures as database errors.
fn scope_error(err: diesel::result::Error, context: &'static str) -> AppError {
    match err {
        diesel::result::Error::NotFound => AppError::NotFound,
        other => AppError::db(other, context),
    }
}

/// The caller's personal workspace — the default target when a request names none.
///
/// Provisioned by the `users_create_workspace` trigger on insert into `users`, so every
/// user has exactly one and its absence is a database invariant failure, not a 404.
pub async fn personal_workspace(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
) -> ApiResult<Workspace> {
    workspace::table
        .filter(workspace::user_id.eq(user_id))
        .filter(workspace::kind.eq("user"))
        .select(Workspace::as_select())
        .first(conn)
        .await
        .map_err(|err| match err {
            diesel::result::Error::NotFound => {
                tracing::error!(%user_id, "user has no personal workspace");
                AppError::Internal
            }
            other => AppError::db(other, "access.personal_workspace"),
        })
}

pub async fn authorize_workspace(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    workspace_id: Uuid,
) -> ApiResult<Workspace> {
    workspace::table
        .inner_join(workspace_member::table.on(workspace_member::workspace_id.eq(workspace::id)))
        .filter(workspace::id.eq(workspace_id))
        .filter(workspace_member::user_id.eq(user_id))
        .select(Workspace::as_select())
        .first(conn)
        .await
        .map_err(|err| scope_error(err, "access.authorize_workspace"))
}

pub async fn authorize_session(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    session_id: Uuid,
) -> ApiResult<Session> {
    session::table
        .inner_join(
            workspace_member::table.on(workspace_member::workspace_id.eq(session::workspace_id)),
        )
        .filter(session::id.eq(session_id))
        .filter(workspace_member::user_id.eq(user_id))
        .select(Session::as_select())
        .first(conn)
        .await
        .map_err(|err| scope_error(err, "access.authorize_session"))
}

pub async fn authorize_thread(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    thread_id: Uuid,
) -> ApiResult<Thread> {
    thread::table
        .inner_join(session::table.on(session::id.eq(thread::session_id)))
        .inner_join(
            workspace_member::table.on(workspace_member::workspace_id.eq(session::workspace_id)),
        )
        .filter(thread::id.eq(thread_id))
        .filter(workspace_member::user_id.eq(user_id))
        .select(Thread::as_select())
        .first(conn)
        .await
        .map_err(|err| scope_error(err, "access.authorize_thread"))
}

pub async fn authorize_run(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    run_id: Uuid,
) -> ApiResult<Run> {
    run::table
        .inner_join(thread::table.on(thread::id.eq(run::thread_id)))
        .inner_join(session::table.on(session::id.eq(thread::session_id)))
        .inner_join(
            workspace_member::table.on(workspace_member::workspace_id.eq(session::workspace_id)),
        )
        .filter(run::id.eq(run_id))
        .filter(workspace_member::user_id.eq(user_id))
        .select(Run::as_select())
        .first(conn)
        .await
        .map_err(|err| scope_error(err, "access.authorize_run"))
}

/// The same walk as [`authorize_run`], one level deeper: an exchange hangs off
/// the run whose call it records.
pub async fn authorize_exchange(
    conn: &mut AsyncPgConnection,
    user_id: Uuid,
    exchange_id: Uuid,
) -> ApiResult<Exchange> {
    exchange::table
        .inner_join(run::table.on(run::id.eq(exchange::run_id)))
        .inner_join(thread::table.on(thread::id.eq(run::thread_id)))
        .inner_join(session::table.on(session::id.eq(thread::session_id)))
        .inner_join(
            workspace_member::table.on(workspace_member::workspace_id.eq(session::workspace_id)),
        )
        .filter(exchange::id.eq(exchange_id))
        .filter(workspace_member::user_id.eq(user_id))
        .select(Exchange::as_select())
        .first(conn)
        .await
        .map_err(|err| scope_error(err, "access.authorize_exchange"))
}
