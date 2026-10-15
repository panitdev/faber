//! The Environments plugin's config: one ordered list of environments.
//!
//! ```ts
//! type EnvironmentsConfig = { environments: Environment[] };
//! type Environment =
//!   | { id: EnvId; name: string; enabled: boolean; type: "scratch" }
//!   | { id: EnvId; name: string; enabled: boolean; type: "user"; environmentId: EnvironmentId };
//! ```
//!
//! The order is the order the agent sees the environments in. `id` is stable
//! through renames, so a change is described by what happened to an entry
//! rather than as one name leaving and another arriving; `name` is what the
//! agent passes as `execute_in`. Code finds scratch by its type, never by its
//! name, so no name is reserved.
//!
//! A `user` entry's `environmentId` is any environment the project's owner
//! registered: a container's id, or a host's id for a host used directly. The
//! owner check happens at bind, since validation is pure.

use std::collections::BTreeSet;

use plugin::{ConfigError, Json, Message};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

/// The config version this plugin writes.
pub const VERSION: u32 = 2;

/// The longest id or name an entry may have.
const MAX_LEN: usize = 64;

/// What scratch tells the agent about itself, in the head when the session
/// starts with scratch and as a notice when it is turned on mid-session,
/// under the name its config entry gives it.
pub fn scratch_note(name: &str) -> String {
    format!(
        "`{name}` belongs to this session. Its files are kept while the session lives; mounts under /mnt are live views, not copies."
    )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentsConfig {
    pub environments: Vec<EnvironmentConfig>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The session's own sandbox. At most one per config.
    Scratch,
    /// An environment the project's owner registered with Faber.
    User,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Scratch => "scratch",
            Kind::User => "user",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentConfig {
    /// Stable across renames.
    pub id: String,
    /// Unique across every entry, enabled or not; the `execute_in` value.
    pub name: String,
    pub enabled: bool,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Set exactly when `kind` is `user`; validation sees to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<Uuid>,
}

impl EnvironmentsConfig {
    pub fn parse(config: &Json) -> Result<Self, Vec<ConfigError>> {
        serde_json::from_value(config.clone()).map_err(|error| {
            vec![ConfigError::whole(format!(
                "not an environments config: {error}"
            ))]
        })
    }

    /// The entry the agent means by `name`, enabled or not.
    pub fn named(&self, name: &str) -> Option<&EnvironmentConfig> {
        self.environments.iter().find(|entry| entry.name == name)
    }

    /// The scratch entry, found by type.
    pub fn scratch(&self) -> Option<&EnvironmentConfig> {
        self.environments
            .iter()
            .find(|entry| entry.kind == Kind::Scratch)
    }

    /// Whether the session gets a scratch at all.
    pub fn scratch_enabled(&self) -> bool {
        self.scratch().is_some_and(|entry| entry.enabled)
    }

    /// The name the agent knows scratch by, when it is turned on.
    pub fn scratch_name(&self) -> Option<&str> {
        self.scratch()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.name.as_str())
    }

    /// The entries the agent is told about, in config order.
    pub fn enabled(&self) -> impl Iterator<Item = &EnvironmentConfig> {
        self.environments.iter().filter(|entry| entry.enabled)
    }

    /// Every name the agent can use, in config order.
    pub fn names(&self) -> Vec<String> {
        self.enabled().map(|entry| entry.name.clone()).collect()
    }
}

pub fn default() -> Json {
    json!({
        "environments": [
            { "id": "scratch", "name": "scratch", "enabled": true, "type": "scratch" }
        ]
    })
}

pub fn schema() -> Json {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "environments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string", "minLength": 1, "maxLength": MAX_LEN },
                        "name": { "type": "string", "minLength": 1, "maxLength": MAX_LEN },
                        "enabled": { "type": "boolean" },
                        "type": { "enum": ["scratch", "user"] },
                        "environmentId": { "type": "string", "minLength": 36, "maxLength": 36 }
                    },
                    "required": ["id", "name", "enabled", "type"]
                }
            }
        },
        "required": ["environments"]
    })
}

/// Checks and normalizes a config: every rule the shape cannot express is
/// checked with a pointer to the field it is about.
pub fn validate(config: Json) -> Result<Json, Vec<ConfigError>> {
    let violations = plugin::schema::check(&schema(), &config);
    if !violations.is_empty() {
        return Err(violations
            .into_iter()
            .map(|violation| ConfigError::new(violation.path, violation.message))
            .collect());
    }

    let mut errors = Vec::new();
    // `environmentId` is checked here, from the raw value, so a malformed id
    // points at its field instead of failing the whole parse.
    let entries = config["environments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for (index, entry) in entries.iter().enumerate() {
        let at = |field: &str| format!("/environments/{index}/{field}");
        let kind = entry["type"].as_str().unwrap_or_default();
        match (kind, entry.get("environmentId").and_then(Json::as_str)) {
            ("user", None) => errors.push(ConfigError::new(
                at("environmentId"),
                "a `user` environment names the registered environment it uses",
            )),
            ("user", Some(id)) if id.parse::<Uuid>().is_err() => {
                errors.push(ConfigError::new(at("environmentId"), "is not an id"))
            }
            ("scratch", Some(_)) => errors.push(ConfigError::new(
                at("environmentId"),
                "scratch is Faber's own and names no registered environment",
            )),
            _ => {}
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let parsed = EnvironmentsConfig::parse(&config)?;
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut scratch = false;
    for (index, entry) in parsed.environments.iter().enumerate() {
        let at = |field: &str| format!("/environments/{index}/{field}");
        if !ids.insert(entry.id.as_str()) {
            errors.push(ConfigError::new(
                at("id"),
                format!("`{}` is used twice", entry.id),
            ));
        }
        if !is_name(&entry.name) {
            errors.push(ConfigError::new(
                at("name"),
                "use letters, digits, `.`, `_` and `-`, starting with a letter or digit",
            ));
        } else if !names.insert(entry.name.as_str()) {
            errors.push(ConfigError::new(
                at("name"),
                format!("`{}` is used twice", entry.name),
            ));
        }
        if entry.kind == Kind::Scratch {
            if scratch {
                errors.push(ConfigError::new(
                    at("type"),
                    "a project has at most one scratch",
                ));
            }
            scratch = true;
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(serde_json::to_value(parsed).expect("an environments config serializes"))
}

/// Names: no separators, no dot-only names, no option look-alikes.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_LEN
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What the agent is told when environments change mid-session, matched by
/// stable id.
///
/// A disabled entry is invisible to the agent, so what happens to it while it
/// stays disabled is not said at all, and switching one on or off is said as
/// such rather than as an arrival or a departure. What a newly visible
/// environment is follows from `open`, which posts the current list.
pub fn changes(old: &EnvironmentsConfig, new: &EnvironmentsConfig) -> Vec<String> {
    let mut said = Vec::new();

    for entry in &new.environments {
        let before = old.environments.iter().find(|before| before.id == entry.id);
        match before {
            None if entry.enabled => said.push(format!("`{}` was added", entry.name)),
            None => {}
            Some(before) => match (before.enabled, entry.enabled) {
                (false, false) => {}
                (false, true) => said.push(format!("`{}` was turned on", entry.name)),
                (true, false) => said.push(format!("`{}` was turned off", before.name)),
                (true, true) => {
                    if before.name != entry.name {
                        said.push(format!(
                            "`{}` was renamed to `{}`; use the new name",
                            before.name, entry.name
                        ));
                    }
                    if before.environment_id != entry.environment_id {
                        said.push(format!(
                            "`{}` now points at a different environment",
                            entry.name
                        ));
                    }
                }
            },
        }
    }
    for entry in &old.environments {
        if entry.enabled && !new.environments.iter().any(|after| after.id == entry.id) {
            said.push(format!("`{}` was removed", entry.name));
        }
    }
    said
}

/// The notice-shaped form of [`changes`]. When scratch becomes available
/// mid-session the note it would have had in the head comes with it, once.
pub fn changed_message(old: &EnvironmentsConfig, new: &EnvironmentsConfig) -> Option<Message> {
    let said = changes(old, new);
    if said.is_empty() {
        return None;
    }
    let mut text = format!(
        "The user changed this project's environments: {}.",
        said.join("; ")
    );
    if !old.scratch_enabled()
        && let Some(name) = new.scratch_name()
    {
        text.push(' ');
        text.push_str(&scratch_note(name));
    }
    Some(Message::user(format!(
        "<environments>{text}</environments>"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOMELAB: &str = "0191b4f2-7c3a-7000-8000-000000000001";
    const GPU: &str = "0191b4f2-7c3a-7000-8000-000000000002";

    fn scratch(enabled: bool) -> Json {
        json!({ "id": "scratch", "name": "scratch", "enabled": enabled, "type": "scratch" })
    }

    fn user(id: &str, name: &str, enabled: bool, environment: &str) -> Json {
        json!({ "id": id, "name": name, "enabled": enabled, "type": "user", "environmentId": environment })
    }

    fn parse(environments: Vec<Json>) -> EnvironmentsConfig {
        EnvironmentsConfig::parse(&json!({ "environments": environments })).unwrap()
    }

    fn paths(errors: &[ConfigError]) -> Vec<&str> {
        errors.iter().map(|error| error.path.as_str()).collect()
    }

    #[test]
    fn the_default_is_scratch_alone_and_normalizes_to_itself() {
        assert_eq!(validate(default()).unwrap(), default());
        let config = EnvironmentsConfig::parse(&default()).unwrap();
        assert!(config.scratch_enabled());
        assert_eq!(config.names(), ["scratch"]);
    }

    #[test]
    fn a_full_list_validates_in_its_own_order() {
        let config = json!({ "environments": [
            user("a", "homelab", true, HOMELAB),
            scratch(true),
            user("b", "gpu-box", false, GPU),
        ] });
        assert_eq!(validate(config.clone()).unwrap(), config);
        let parsed = EnvironmentsConfig::parse(&config).unwrap();
        assert_eq!(parsed.names(), ["homelab", "scratch"]);
        assert_eq!(parsed.named("gpu-box").unwrap().kind, Kind::User);
    }

    #[test]
    fn every_error_points_at_its_field() {
        let errors = validate(json!({ "environments": [
            scratch(true),
            user("a", "a b", true, HOMELAB),
            user("a", "homelab", true, HOMELAB),
            user("c", "homelab", false, GPU),
            json!({ "id": "s2", "name": "scratch2", "enabled": false, "type": "scratch" }),
        ] }))
        .unwrap_err();
        let paths = paths(&errors);
        assert!(paths.contains(&"/environments/1/name"), "{paths:?}");
        assert!(paths.contains(&"/environments/2/id"), "{paths:?}");
        assert!(paths.contains(&"/environments/3/name"), "{paths:?}");
        assert!(paths.contains(&"/environments/4/type"), "{paths:?}");
    }

    #[test]
    fn names_are_unique_across_disabled_entries_and_none_is_reserved() {
        // Someone else's name for scratch is fine; scratch is found by type.
        let config = json!({ "environments": [
            json!({ "id": "s", "name": "sandbox", "enabled": true, "type": "scratch" }),
            user("a", "scratch", true, HOMELAB),
        ] });
        assert!(validate(config).is_ok());

        let errors = validate(json!({ "environments": [
            user("a", "box", false, HOMELAB),
            user("b", "box", true, GPU),
        ] }))
        .unwrap_err();
        assert_eq!(paths(&errors), ["/environments/1/name"]);
    }

    #[test]
    fn environment_ids_belong_to_user_entries_only() {
        let errors = validate(json!({ "environments": [
            json!({ "id": "a", "name": "a", "enabled": true, "type": "user" }),
            user("b", "b", true, "not-a-uuid-but-exactly-36-characters"),
            json!({ "id": "s", "name": "s", "enabled": true, "type": "scratch", "environmentId": HOMELAB }),
        ] }))
        .unwrap_err();
        assert_eq!(
            paths(&errors),
            [
                "/environments/0/environmentId",
                "/environments/1/environmentId",
                "/environments/2/environmentId"
            ]
        );
    }

    #[test]
    fn the_shape_is_checked_with_pointers_too() {
        let errors = validate(json!({ "environments": [
            json!({ "id": "a", "name": "a", "type": "user", "environmentId": HOMELAB, "workdir": "/x" }),
        ] }))
        .unwrap_err();
        assert_eq!(
            paths(&errors),
            ["/environments/0/enabled", "/environments/0/workdir"]
        );

        let errors =
            validate(json!({ "scratch": { "enabled": true }, "machines": [] })).unwrap_err();
        assert!(!errors.is_empty());
    }

    #[test]
    fn changes_are_said_by_stable_id() {
        let old = parse(vec![
            scratch(true),
            user("a", "laptop", true, HOMELAB),
            user("b", "build", true, GPU),
            user("d", "old", true, GPU),
        ]);
        let new = parse(vec![
            scratch(false),
            user("a", "work", true, HOMELAB),
            user("b", "build", true, HOMELAB),
            user("c", "gpu", true, GPU),
        ]);
        assert_eq!(
            changes(&old, &new),
            [
                "`scratch` was turned off",
                "`laptop` was renamed to `work`; use the new name",
                "`build` now points at a different environment",
                "`gpu` was added",
                "`old` was removed",
            ]
        );
        assert!(changed_message(&old, &old).is_none());
    }

    #[test]
    fn disabled_entries_are_silent_until_turned_on() {
        let old = parse(vec![user("a", "gpu-box", false, HOMELAB)]);
        let renamed = parse(vec![
            user("a", "gpu", false, GPU),
            user("b", "spare", false, GPU),
        ]);
        assert!(changes(&old, &renamed).is_empty());
        assert!(changes(&renamed, &parse(vec![])).is_empty());

        let on = parse(vec![user("a", "gpu", true, GPU)]);
        assert_eq!(changes(&renamed, &on)[0], "`gpu` was turned on");
    }

    const NOTE: &str = "`scratch` belongs to this session. Its files are kept while the session lives; mounts under /mnt are live views, not copies.";

    #[test]
    fn scratch_turned_on_brings_its_note_once() {
        let off = parse(vec![scratch(false)]);
        let on = parse(vec![scratch(true)]);
        assert_eq!(
            changed_message(&off, &on).unwrap().text(),
            format!(
                "<environments>The user changed this project's environments: `scratch` was turned on. {NOTE}</environments>"
            )
        );
        assert_eq!(
            changed_message(&on, &off).unwrap().text(),
            "<environments>The user changed this project's environments: `scratch` was turned off.</environments>"
        );

        let added = changed_message(&parse(vec![]), &on).unwrap().text();
        assert!(added.contains(NOTE), "{added}");
    }

    #[test]
    fn a_scratch_with_another_name_is_called_by_it() {
        let off = parse(vec![
            json!({ "id": "s", "name": "sandbox", "enabled": false, "type": "scratch" }),
        ]);
        let on = parse(vec![
            json!({ "id": "s", "name": "sandbox", "enabled": true, "type": "scratch" }),
        ]);
        assert_eq!(on.scratch_name(), Some("sandbox"));
        assert_eq!(off.scratch_name(), None);
        let message = changed_message(&off, &on).unwrap().text();
        assert!(
            message.contains("`sandbox` was turned on. `sandbox` belongs to this session."),
            "{message}"
        );
        assert!(!message.contains("`scratch`"), "{message}");
    }
}
