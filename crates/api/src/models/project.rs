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
    /// The owner's limit on runs of spawned sessions at once; `None` is the
    /// operator's ceiling.
    pub max_agent_runs: Option<i32>,
    /// The owner's limit on open sessions one session may have spawned.
    pub max_session_agents: Option<i32>,
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
    /// The owner's model alias the sessions this binding opens run on.
    pub model_alias: Option<String>,
    /// Their thinking selection, as a session stores one.
    pub thinking_effort: Option<String>,
    /// The owner's limits on what this binding may start; `None` is no limit
    /// beyond the operator's ceiling, if one is set.
    pub max_sessions: Option<i32>,
    pub max_runs: Option<i32>,
    pub messages_per_minute: Option<i32>,
    /// The plugins the sessions this binding opens may use, besides itself;
    /// `None` is every plugin of the project.
    pub session_plugins: Option<Vec<String>>,
}

/// The bindings a session's run uses. A session a binding opened gets only
/// the plugins that binding allows its sessions, and always the binding
/// itself; every other session gets them all.
pub fn narrowed(rows: Vec<ProjectBinding>, opener: Option<&str>) -> Vec<ProjectBinding> {
    let Some(opener) = opener else {
        return rows;
    };
    let Some(allowed) = rows
        .iter()
        .find(|row| row.plugin_type == opener)
        .and_then(|row| row.session_plugins.clone())
    else {
        return rows;
    };
    rows.into_iter()
        .filter(|row| row.plugin_type == opener || allowed.contains(&row.plugin_type))
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(plugin: &str, session_plugins: Option<&[&str]>) -> ProjectBinding {
        ProjectBinding {
            project_id: Uuid::nil(),
            plugin_type: plugin.to_owned(),
            version: "1.0.0".to_owned(),
            config_version: 1,
            config: serde_json::json!({}),
            enabled: true,
            created_at: Utc::now(),
            model_alias: None,
            thinking_effort: None,
            max_sessions: None,
            max_runs: None,
            messages_per_minute: None,
            session_plugins: session_plugins
                .map(|plugins| plugins.iter().map(|p| (*p).to_owned()).collect()),
        }
    }

    fn types(rows: Vec<ProjectBinding>) -> Vec<String> {
        rows.into_iter().map(|row| row.plugin_type).collect()
    }

    #[test]
    fn a_binding_narrows_only_the_sessions_it_opened() {
        let rows = || {
            vec![
                row("environments", None),
                row("web", None),
                row("discord", Some(&["web"])),
            ]
        };
        assert_eq!(types(narrowed(rows(), Some("discord"))), ["web", "discord"]);
        // The owner's sessions, and a binding with no list, keep everything.
        assert_eq!(types(narrowed(rows(), None)).len(), 3);
        assert_eq!(types(narrowed(rows(), Some("web"))).len(), 3);
        // An empty list leaves the binding alone with its own tools.
        let alone = vec![row("environments", None), row("discord", Some(&[]))];
        assert_eq!(types(narrowed(alone, Some("discord"))), ["discord"]);
    }
}
