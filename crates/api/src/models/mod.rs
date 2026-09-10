pub mod agent;
pub mod blob;
pub mod credential;
pub mod exchange;
pub mod host;
pub mod model_config;
#[allow(dead_code)]
pub mod presentation;
pub mod run;
pub mod session;
pub mod span;
pub mod spine;
pub mod thinking;
pub mod thread;
pub mod transcript;
pub mod user;
pub mod workspace;

/// Current time as epoch seconds.
///
/// The conversation tables store `BIGINT` epoch seconds rather than the `TIMESTAMPTZ`
/// that `users`, `credentials`, and `models` use. This is the single source for that
/// half of the schema — `202608050001_agent/up.sql` seeds the same way.
pub fn now_epoch() -> i64 {
    chrono::Utc::now().timestamp()
}
