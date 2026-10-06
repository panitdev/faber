//! The Environments plugin's config: scratch, and the user's machines.
//!
//! ```ts
//! type EnvironmentsConfig = {
//!   scratch: { enabled: boolean; repos: RepoRef[] };
//!   machines: Machine[];
//! };
//! type Machine = { id: EnvId; name: string; agentId: AgentId; workdir?: AbsolutePath };
//! type RepoRef = { url: string; ref?: string; dir: string };  // cloned to /repos/<dir>
//! ```
//!
//! Scratch quotas come from the user's plan, not from config.

use std::collections::BTreeSet;

use plugin::{ConfigError, Json, Message};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

/// The name scratch is addressed by, which no machine may take.
pub const SCRATCH: &str = "scratch";

/// The config version this plugin writes.
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentsConfig {
    #[serde(default)]
    pub scratch: ScratchConfig,
    #[serde(default)]
    pub machines: Vec<MachineConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScratchConfig {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub repos: Vec<RepoRef>,
}

fn enabled() -> bool {
    true
}

impl Default for ScratchConfig {
    fn default() -> Self {
        ScratchConfig {
            enabled: true,
            repos: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MachineConfig {
    /// Stable across renames.
    pub id: String,
    /// Unique; `scratch` is reserved.
    pub name: String,
    /// The faber-agent instance: an agent-transport host the project's owner
    /// enrolled.
    pub agent_id: Uuid,
    /// This project's directory on the machine: shown to the agent, and the
    /// default `cwd` for commands there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepoRef {
    pub url: String,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Cloned to `/repos/<dir>`.
    pub dir: String,
}

impl EnvironmentsConfig {
    pub fn parse(config: &Json) -> Result<Self, Vec<ConfigError>> {
        serde_json::from_value(config.clone()).map_err(|error| {
            vec![ConfigError::whole(format!(
                "not an environments config: {error}"
            ))]
        })
    }

    pub fn machine(&self, name: &str) -> Option<&MachineConfig> {
        self.machines.iter().find(|machine| machine.name == name)
    }

    /// Every environment name the agent can use, scratch first.
    pub fn names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.scratch.enabled {
            names.push(SCRATCH.to_owned());
        }
        names.extend(self.machines.iter().map(|machine| machine.name.clone()));
        names
    }
}

pub fn default() -> Json {
    json!({ "scratch": { "enabled": true, "repos": [] }, "machines": [] })
}

pub fn schema() -> Json {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "scratch": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "enabled": { "type": "boolean" },
                    "repos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "url": { "type": "string", "minLength": 1 },
                                "ref": { "type": "string", "minLength": 1 },
                                "dir": { "type": "string", "minLength": 1 }
                            },
                            "required": ["url", "dir"]
                        }
                    }
                }
            },
            "machines": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string", "minLength": 1 },
                        "name": { "type": "string", "minLength": 1, "maxLength": 64 },
                        "agentId": { "type": "string", "minLength": 36, "maxLength": 36 },
                        "workdir": { "type": "string", "minLength": 1 }
                    },
                    "required": ["id", "name", "agentId"]
                }
            }
        }
    })
}

/// Checks and normalizes a config: defaults filled in, every rule the shape
/// cannot express checked with a pointer to where it broke.
pub fn validate(config: Json) -> Result<Json, Vec<ConfigError>> {
    let violations = plugin::schema::check(&schema(), &config);
    if !violations.is_empty() {
        return Err(violations
            .into_iter()
            .map(|violation| ConfigError::new(violation.path, violation.message))
            .collect());
    }
    let parsed = EnvironmentsConfig::parse(&config)?;
    let mut errors = Vec::new();

    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for (index, machine) in parsed.machines.iter().enumerate() {
        let at = |field: &str| format!("/machines/{index}/{field}");
        if !ids.insert(machine.id.as_str()) {
            errors.push(ConfigError::new(
                at("id"),
                format!("`{}` is used twice", machine.id),
            ));
        }
        if machine.name == SCRATCH {
            errors.push(ConfigError::new(
                at("name"),
                "`scratch` is reserved for the project's sandbox",
            ));
        } else if !is_name(&machine.name) {
            errors.push(ConfigError::new(
                at("name"),
                "use letters, digits, `.`, `_` and `-`, starting with a letter or digit",
            ));
        }
        if !names.insert(machine.name.as_str()) {
            errors.push(ConfigError::new(
                at("name"),
                format!("`{}` is used twice", machine.name),
            ));
        }
        if let Some(workdir) = &machine.workdir
            && (!workdir.starts_with('/') || workdir.contains('\0'))
        {
            errors.push(ConfigError::new(at("workdir"), "must be an absolute path"));
        }
    }

    let mut dirs = BTreeSet::new();
    for (index, repo) in parsed.scratch.repos.iter().enumerate() {
        let at = |field: &str| format!("/scratch/repos/{index}/{field}");
        if !is_repo_url(&repo.url) {
            errors.push(ConfigError::new(
                at("url"),
                "use an https://, ssh:// or git@host: URL",
            ));
        }
        if let Some(reference) = &repo.reference
            && (reference.starts_with('-')
                || reference
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control()))
        {
            errors.push(ConfigError::new(at("ref"), "is not a git ref"));
        }
        if !is_name(&repo.dir) {
            errors.push(ConfigError::new(
                at("dir"),
                "must be one directory name: letters, digits, `.`, `_` and `-`",
            ));
        }
        if !dirs.insert(repo.dir.as_str()) {
            errors.push(ConfigError::new(
                at("dir"),
                format!("`{}` is used twice", repo.dir),
            ));
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(serde_json::to_value(parsed).expect("an environments config serializes"))
}

/// Names and repo directories: no separators, no dot-only names, no option
/// look-alikes.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Remote URLs only. A `file://` URL or a bare path would clone from the
/// Faber server's own disk.
fn is_repo_url(url: &str) -> bool {
    let remote = url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("ssh://")
        || (url.starts_with("git@") && url.contains(':'));
    remote && !url.starts_with('-') && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// What the agent is told when environments change mid-session: names, not
/// manifests (those follow from `open`).
pub fn changes(old: &EnvironmentsConfig, new: &EnvironmentsConfig) -> Vec<String> {
    let mut said = Vec::new();

    match (old.scratch.enabled, new.scratch.enabled) {
        (false, true) => said.push("`scratch` was added".to_owned()),
        (true, false) => said.push("`scratch` was removed".to_owned()),
        _ => {}
    }
    if new.scratch.enabled && old.scratch.repos != new.scratch.repos {
        let dirs: Vec<String> = new
            .scratch
            .repos
            .iter()
            .map(|repo| format!("/repos/{}", repo.dir))
            .collect();
        said.push(if dirs.is_empty() {
            "scratch no longer has configured repos".to_owned()
        } else {
            format!("scratch's repos are now {}", dirs.join(", "))
        });
    }

    for machine in &new.machines {
        match old.machines.iter().find(|before| before.id == machine.id) {
            None => said.push(format!("machine `{}` was added", machine.name)),
            Some(before) => {
                if before.name != machine.name {
                    said.push(format!(
                        "machine `{}` was renamed to `{}`; use the new name",
                        before.name, machine.name
                    ));
                }
                if before.agent_id != machine.agent_id {
                    said.push(format!(
                        "machine `{}` now points at a different computer",
                        machine.name
                    ));
                }
                if before.workdir != machine.workdir {
                    said.push(format!(
                        "machine `{}`'s workdir is now {}",
                        machine.name,
                        machine.workdir.as_deref().unwrap_or("unset")
                    ));
                }
            }
        }
    }
    for machine in &old.machines {
        if !new.machines.iter().any(|after| after.id == machine.id) {
            said.push(format!("machine `{}` was removed", machine.name));
        }
    }
    said
}

/// The notice-shaped form of [`changes`].
pub fn changed_message(old: &EnvironmentsConfig, new: &EnvironmentsConfig) -> Option<Message> {
    let said = changes(old, new);
    if said.is_empty() {
        return None;
    }
    Some(Message::user(format!(
        "<environments>The user changed this project's environments: {}.</environments>",
        said.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(id: &str, name: &str) -> Json {
        json!({ "id": id, "name": name, "agentId": "0191b4f2-7c3a-7000-8000-000000000001" })
    }

    #[test]
    fn the_default_is_valid_and_normalizes_to_itself() {
        assert_eq!(validate(default()).unwrap(), default());
        assert_eq!(validate(json!({})).unwrap(), default());
    }

    #[test]
    fn rules_the_schema_cannot_express_are_checked_with_a_pointer() {
        let errors = validate(json!({
            "scratch": { "repos": [
                { "url": "file:///etc", "dir": "../x" },
                { "url": "https://github.com/a/b", "ref": "--upload-pack=x", "dir": "b" }
            ] },
            "machines": [machine("1", "scratch"), machine("1", "a b"), machine("2", "ok"), machine("3", "ok")]
        }))
        .unwrap_err();
        let paths: Vec<&str> = errors.iter().map(|error| error.path.as_str()).collect();
        assert!(paths.contains(&"/machines/0/name"));
        assert!(paths.contains(&"/machines/1/id"));
        assert!(paths.contains(&"/machines/1/name"));
        assert!(paths.contains(&"/machines/3/name"));
        assert!(paths.contains(&"/scratch/repos/0/url"));
        assert!(paths.contains(&"/scratch/repos/0/dir"));
        assert!(paths.contains(&"/scratch/repos/1/ref"));
    }

    #[test]
    fn unknown_fields_are_refused() {
        let errors = validate(json!({ "machine": [] })).unwrap_err();
        assert!(errors[0].message.contains("not allowed"));
    }

    #[test]
    fn changes_name_what_changed_by_stable_id() {
        let old = EnvironmentsConfig::parse(&json!({
            "machines": [machine("a", "laptop"), machine("b", "build")]
        }))
        .unwrap();
        let new = EnvironmentsConfig::parse(&json!({
            "scratch": { "enabled": false },
            "machines": [machine("a", "work"), machine("c", "gpu")]
        }))
        .unwrap();
        assert_eq!(
            changes(&old, &new),
            [
                "`scratch` was removed",
                "machine `laptop` was renamed to `work`; use the new name",
                "machine `gpu` was added",
                "machine `build` was removed",
            ]
        );
        assert!(changed_message(&old, &old).is_none());
    }
}
