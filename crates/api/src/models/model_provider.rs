//! Providers a model preset is served by.
//!
//! A provider is metadata in models.dev's shape — a key, a display name, its
//! documentation, the AI SDK package that speaks its API, and the endpoint it
//! states. Nothing here routes a request. It is owned by a user or by the system (`user_id IS NULL`),
//! and a preset references one. The nullable owner is what lets the two halves
//! share a table while a per-user listing reads both and CRUD touches only the
//! user's rows.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use crate::schema::model_providers;

/// One provider as stored. Field order matches the table's column order, which
/// `#[derive(Selectable)]` requires.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = model_providers)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ModelProviderRow {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    /// The publisher's key, e.g. `anthropic`.
    pub provider_id: String,
    pub name: String,
    /// The provider's model documentation.
    pub doc: Option<String>,
    /// An OpenAI-compatible endpoint, when the provider states one.
    pub api: Option<String>,
    pub created_at: DateTime<Utc>,
    /// The AI SDK package that speaks this provider's API.
    pub npm: Option<String>,
    /// Environment variable names upstream reads the key from, as a JSON array.
    /// Informational: this service never reads a key from its environment.
    pub env: Value,
}

impl ModelProviderRow {
    pub fn env(&self) -> Vec<String> {
        crate::models::creator_model::from_db(&self.env)
    }
}

/// One provider as it is written. `user_id` is `None` for a system provider.
#[derive(Debug, Insertable)]
#[diesel(table_name = model_providers)]
pub struct NewModelProvider {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub provider_id: String,
    pub name: String,
    pub doc: Option<String>,
    pub api: Option<String>,
    pub npm: Option<String>,
    pub env: Value,
}

/// The fields a `PATCH` may change on a user's provider. The provider key is
/// its identity and is not editable — presets display it.
#[derive(AsChangeset, Default)]
#[diesel(table_name = model_providers)]
pub struct UpdateModelProvider {
    pub name: Option<String>,
    pub doc: Option<Option<String>>,
    pub api: Option<Option<String>>,
    pub npm: Option<Option<String>>,
    pub env: Option<Value>,
}
