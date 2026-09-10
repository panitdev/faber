use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::{session, session_environment, session_ref};

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = session)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Session {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: Option<String>,
    pub created_at: i64,
    pub closed_at: Option<i64>,
    /// Alias of the model the next message goes to. `None` until something
    /// picks one — a session created from the sidebar has no model yet, and
    /// the first message it sends is what settles the question.
    pub model_alias: Option<String>,
    /// The thinking knob as the user left it, in the stored form
    /// [`crate::models::thinking::ThinkingSelection`] reads. `None` means
    /// never picked, which is not the same as picked-and-turned-off.
    pub thinking_effort: Option<String>,
}

#[derive(Insertable)]
#[diesel(table_name = session)]
pub struct NewSession<'a> {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: Option<&'a str>,
    pub created_at: i64,
}

// A new session carries no model or thinking selection: both columns stay
// NULL until a picker or a message names one. Nothing is defaulted here on
// purpose — "the first model in the list" is a client-side convenience, and
// baking it into the row would make a model the user never chose look like
// one they did.

#[derive(AsChangeset, Default)]
#[diesel(table_name = session)]
pub struct UpdateSession<'a> {
    /// `Some(None)` clears the title; `None` leaves it alone.
    pub title: Option<Option<&'a str>>,
    /// `Some(None)` reopens a closed session.
    pub closed_at: Option<Option<i64>>,
    /// `Some(None)` clears the model selection, leaving the session with
    /// nothing to send to until something picks again.
    pub model_alias: Option<Option<&'a str>>,
    /// `Some(None)` clears the thinking selection back to the model's own
    /// default — which is a different state from having picked "off".
    pub thinking_effort: Option<Option<&'a str>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = session_ref)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SessionRef {
    pub token: String,
    pub session_id: Uuid,
    pub issued_at: i64,
    pub revoked_at: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = session_ref)]
pub struct NewSessionRef<'a> {
    pub token: &'a str,
    pub session_id: Uuid,
    pub issued_at: i64,
}

/// One environment a session may address, by the label the user tagged it
/// with.
///
/// Append-only: `removed_at` tombstones a binding and nothing deletes the row,
/// because the label stays claimed. A transcript records calls as
/// `label:path`, so letting a label mean a second machine later would make the
/// earlier half of that transcript quietly wrong.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = session_environment)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SessionEnvironment {
    pub session_id: Uuid,
    pub label: String,
    pub host_id: Uuid,
    /// `None` means the host itself, in direct exec mode.
    pub container_id: Option<Uuid>,
    pub added_at: i64,
    pub removed_at: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = session_environment)]
pub struct NewSessionEnvironment<'a> {
    pub session_id: Uuid,
    pub label: &'a str,
    pub host_id: Uuid,
    pub container_id: Option<Uuid>,
    pub added_at: i64,
}
