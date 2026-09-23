//! Providers a model preset is published by.
//!
//! A provider is metadata — a key, a display name, and where it says it is
//! served from. It is owned by a user or by the system (`user_id IS NULL`),
//! and a preset references one. The nullable owner is what lets the two halves
//! share a table while a per-user listing reads both and CRUD touches only the
//! user's rows.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
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
    pub website: Option<String>,
    pub api_base_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One provider as it is written. `user_id` is `None` for a system provider.
#[derive(Debug, Insertable)]
#[diesel(table_name = model_providers)]
pub struct NewModelProvider {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub provider_id: String,
    pub name: String,
    pub website: Option<String>,
    pub api_base_url: Option<String>,
}

/// The fields a `PATCH` may change on a user's provider. The provider key is
/// its identity and is not editable — presets display it.
#[derive(AsChangeset, Default)]
#[diesel(table_name = model_providers)]
pub struct UpdateModelProvider {
    pub name: Option<String>,
    pub website: Option<Option<String>>,
    pub api_base_url: Option<Option<String>>,
}
