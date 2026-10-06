use chrono::{DateTime, Utc};
use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::{project, project_binding};

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = project)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Project {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub description: String,
    /// Increments on any change to the project or its bindings.
    pub rev: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = project)]
pub struct NewProject<'a> {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: &'a str,
    pub description: &'a str,
}

#[derive(Debug, Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = project_binding)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProjectBinding {
    pub project_id: Uuid,
    pub plugin_type: String,
    pub version: String,
    pub config_version: i32,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

impl ProjectBinding {
    /// The core's view of the row.
    pub fn to_binding(&self) -> plugin::Binding {
        plugin::Binding {
            plugin: self.plugin_type.clone(),
            version: self.version.clone(),
            config_version: self.config_version.max(1) as u32,
            config: self.config.clone(),
            enabled: self.enabled,
            order: self.created_at.timestamp_micros(),
        }
    }
}
