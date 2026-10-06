//! Environments: the built-in plugin that owns everything about files and
//! commands.
//!
//! The agent names the environment on every call, and paths are that
//! environment's real paths; there is no namespace shared across environments.
//! A project has `scratch` (a Faber-provided sandbox, a cache rather than
//! storage) and the user's own machines running faber-agent.
//!
//! - **System head:** `open(fresh)` returns each environment's manifest and a
//!   note that scratch is not durable.
//! - **Mid-session changes:** `on-changed` names what changed, and
//!   `open(reopened)` posts the current manifests as a notice.
//! - **Notices:** posted only during this plugin's own calls. Each call checks
//!   the project's machines and posts any status change since it last reported
//!   in the session, withdrawing an offline notice that has not landed yet if
//!   the machine is back; process exits are reported when a call observes them.

pub mod config;
pub mod places;
pub mod tools;

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use diesel::{
    ExpressionMethods, JoinOnDsl, NullableExpressionMethods, OptionalExtension, QueryDsl,
    SelectableHelper,
};
use diesel_async::RunQueryDsl;
use environment::ProcId;
use plugin::{
    ConfigError, Content, Delivery, Json, Manifest, Message, Pending, Phase, Plugin, SessionCtx,
    ToolCall, ToolResult,
};
use uuid::Uuid;

use self::config::{EnvironmentsConfig, SCRATCH};
use self::places::{Bound, Places, Unavailable};
use crate::db::DbPool;
use crate::models::host::HostProbe;
use crate::schema::{host, host_probe, project, session};

pub const ID: &str = "environments";

/// The interface dependents import instead of calling these tools.
pub const HANDLE: &str = "faber:environments/handle";

/// How a status or exit notice lands: after the next tool result, or before
/// the user's next message if the model stops first.
const STATUS_DELIVERY: Delivery = Delivery::AfterToolOrMessage;

pub struct Environments {
    manifest: Manifest,
    places: Arc<Places>,
    db: DbPool,
    /// Per-session cache, keyed by session id: who owns the session's project,
    /// and what was last reported to it. Losing it costs a repeated status
    /// notice, never a wrong answer.
    sessions: tokio::sync::Mutex<HashMap<String, SessionState>>,
}

struct SessionState {
    project: Uuid,
    owner: Uuid,
    reported: HashMap<String, Reported>,
    watching: Vec<(Weak<Bound>, ProcId)>,
}

struct Reported {
    online: bool,
    offline_notice: Option<Pending>,
}

impl Environments {
    pub fn new(db: DbPool, places: Arc<Places>) -> Self {
        Environments {
            manifest: manifest(),
            places,
            db,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The session's project and its owner, cached.
    async fn session(&self, ctx: &SessionCtx) -> Result<(Uuid, Uuid), String> {
        if let Some(state) = self.sessions.lock().await.get(ctx.session_id()) {
            return Ok((state.project, state.owner));
        }
        let id: Uuid = ctx
            .session_id()
            .parse()
            .map_err(|_| "this session id is not one Faber issued".to_owned())?;
        let mut conn = self.db.get().await.map_err(|error| error.to_string())?;
        let found: Option<(Uuid, Uuid)> = session::table
            .inner_join(project::table.on(project::id.nullable().eq(session::project_id)))
            .filter(session::id.eq(id))
            .select((project::id, project::owner_id))
            .first(&mut conn)
            .await
            .optional()
            .map_err(|error| error.to_string())?;
        let (project, owner) =
            found.ok_or_else(|| "this session is not in a project".to_owned())?;
        self.sessions.lock().await.insert(
            ctx.session_id().to_owned(),
            SessionState {
                project,
                owner,
                reported: HashMap::new(),
                watching: Vec::new(),
            },
        );
        Ok((project, owner))
    }

    /// Every environment's manifest, from the probe cache. Never a live call.
    async fn manifests(
        &self,
        project: Uuid,
        owner: Uuid,
        config: &EnvironmentsConfig,
    ) -> Vec<Described> {
        let mut described = Vec::new();

        if config.scratch.enabled {
            let bound = self
                .places
                .bound(project)
                .await
                .into_iter()
                .find(|bound| bound.name == SCRATCH);
            let probed = bound.map(|bound| Probe::from(bound.target.manifest()));
            let mut notes = vec![if self.places.scratch_available() {
                "sandboxing: none yet — commands run on the Faber server under its own user"
                    .to_owned()
            } else {
                "not available on this Faber server: calls to it fail".to_owned()
            }];
            if !config.scratch.repos.is_empty() {
                notes.push(format!(
                    "repos: {}",
                    config
                        .scratch
                        .repos
                        .iter()
                        .map(|repo| format!("/repos/{}", repo.dir))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            described.push(Described {
                name: SCRATCH.to_owned(),
                kind: "scratch",
                workdir: "/".to_owned(),
                probe: probed,
                notes,
            });
        }

        let agents: Vec<Uuid> = config
            .machines
            .iter()
            .map(|machine| machine.agent_id)
            .collect();
        let probes: Vec<HostProbe> = match self.db.get().await {
            Ok(mut conn) if !agents.is_empty() => host_probe::table
                .inner_join(host::table.on(host::id.eq(host_probe::host_id)))
                .filter(host::user_id.eq(owner))
                .filter(host_probe::host_id.eq_any(&agents))
                .filter(host_probe::container_id.is_null())
                .filter(host_probe::ok.eq(true))
                .distinct_on(host_probe::host_id)
                .order((host_probe::host_id, host_probe::probed_at.desc()))
                .select(HostProbe::as_select())
                .load(&mut conn)
                .await
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        for machine in &config.machines {
            let probe = probes
                .iter()
                .find(|probe| probe.host_id == machine.agent_id)
                .map(Probe::from);
            described.push(Described {
                name: machine.name.clone(),
                kind: "machine",
                workdir: machine.workdir.clone().unwrap_or_else(|| "/".to_owned()),
                probe,
                notes: vec!["sandboxing: none — the user's own computer".to_owned()],
            });
        }
        described
    }

    /// Posts what changed in machine status since the last report in this
    /// session.
    async fn report_status(&self, ctx: &SessionCtx, config: &EnvironmentsConfig) {
        let mut sessions = self.sessions.lock().await;
        let Some(state) = sessions.get_mut(ctx.session_id()) else {
            return;
        };

        for machine in &config.machines {
            let online = self.places.online(machine.agent_id);
            let reported = state
                .reported
                .entry(machine.id.clone())
                .or_insert(Reported {
                    // What the head implied: there until said otherwise.
                    online: true,
                    offline_notice: None,
                });
            if reported.online == online {
                continue;
            }
            reported.online = online;
            if online {
                // Back before anyone heard it left: say nothing at all.
                if let Some(pending) = reported.offline_notice.take()
                    && pending.withdraw().is_ok()
                {
                    continue;
                }
                post(ctx, format!("Machine `{}` is back online.", machine.name));
            } else {
                reported.offline_notice = post(
                    ctx,
                    format!(
                        "Machine `{}` is offline: its faber-agent is not connected, so calls to it fail until it reconnects.",
                        machine.name
                    ),
                );
            }
        }
    }

    /// Posts exits of processes this session started that have ended since.
    async fn report_exits(&self, ctx: &SessionCtx) {
        let watching = {
            let mut sessions = self.sessions.lock().await;
            let Some(state) = sessions.get_mut(ctx.session_id()) else {
                return;
            };
            std::mem::take(&mut state.watching)
        };

        let mut still = Vec::new();
        for (bound, id) in watching {
            let Some(live) = bound.upgrade() else {
                continue;
            };
            match live.target.processes().await {
                Ok(processes) => match processes.iter().find(|process| process.id == id) {
                    Some(process) => match process.outcome {
                        Some(outcome) => {
                            post(
                                ctx,
                                format!(
                                    "The {}.",
                                    tools::exit_line(&live.name, &live.process_id(id), &outcome)
                                ),
                            );
                        }
                        None => still.push((bound, id)),
                    },
                    None => {}
                },
                Err(_) => still.push((bound, id)),
            }
        }

        if let Some(state) = self.sessions.lock().await.get_mut(ctx.session_id()) {
            state.watching.extend(still);
        }
    }

    async fn resolve(
        &self,
        project: Uuid,
        owner: Uuid,
        config: &EnvironmentsConfig,
        name: &str,
    ) -> Result<(Arc<Bound>, Option<String>, Vec<String>), Unavailable> {
        if name == SCRATCH {
            if !config.scratch.enabled {
                return Err(Unavailable::Missing(
                    "`scratch` is turned off for this project".to_owned(),
                ));
            }
            let (bound, notes) = self.places.scratch(project, &config.scratch.repos).await?;
            return Ok((bound, None, notes));
        }
        let Some(machine) = config.machine(name) else {
            let names = config.names();
            return Err(Unavailable::Missing(format!(
                "no environment is named `{name}`; this project has {}",
                if names.is_empty() {
                    "none".to_owned()
                } else {
                    names
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )));
        };
        let bound = self.places.machine(project, owner, machine).await?;
        Ok((bound, machine.workdir.clone(), Vec::new()))
    }
}

fn post(ctx: &SessionCtx, text: String) -> Option<Pending> {
    match ctx.post(
        Message::user(format!("<environments>{text}</environments>")),
        STATUS_DELIVERY,
    ) {
        Ok(pending) => Some(pending),
        Err(error) => {
            tracing::warn!(%error, "environments notice refused");
            None
        }
    }
}

struct Probe {
    os: String,
    arch: String,
    shell: String,
    tools: Vec<(String, String)>,
}

impl From<&environment::Manifest> for Probe {
    fn from(manifest: &environment::Manifest) -> Self {
        Probe {
            os: manifest.os.clone(),
            arch: manifest.arch.clone(),
            shell: manifest.shell.clone(),
            tools: manifest
                .tools
                .iter()
                .map(|(name, version)| (name.clone(), version.clone()))
                .collect(),
        }
    }
}

impl From<&HostProbe> for Probe {
    fn from(probe: &HostProbe) -> Self {
        Probe {
            os: probe.os.clone().unwrap_or_else(|| "unknown os".into()),
            arch: probe.arch.clone().unwrap_or_else(|| "unknown arch".into()),
            shell: probe.shell.clone().unwrap_or_else(|| "/bin/sh".into()),
            tools: probe
                .tools
                .as_ref()
                .and_then(|tools| tools.as_object())
                .map(|tools| {
                    tools
                        .iter()
                        .map(|(name, version)| {
                            (name.clone(), version.as_str().unwrap_or("").to_owned())
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// One environment as the agent is told about it.
struct Described {
    name: String,
    kind: &'static str,
    workdir: String,
    probe: Option<Probe>,
    notes: Vec<String>,
}

impl Described {
    fn full(&self) -> String {
        let mut line = format!(
            "- `{}` ({}, workdir {})",
            self.name, self.kind, self.workdir
        );
        match &self.probe {
            Some(probe) => {
                line.push_str(&format!(
                    ": {} {}, shell {}",
                    probe.os, probe.arch, probe.shell
                ));
                if !probe.tools.is_empty() {
                    line.push_str(&format!(
                        "; tools found: {}",
                        probe
                            .tools
                            .iter()
                            // Probes record `git version 2.43.0`, not `2.43.0`.
                            .map(|(name, version)| {
                                if version.starts_with(name.as_str()) {
                                    version.clone()
                                } else {
                                    format!("{name} {version}")
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            None => line.push_str(": not probed yet (the first call probes it)"),
        }
        line.push_str("; answers exec, process, read, patch; network access unknown");
        for note in &self.notes {
            line.push_str("; ");
            line.push_str(note);
        }
        line
    }

    fn brief(&self) -> String {
        format!(
            "- `{}` ({}, workdir {})",
            self.name, self.kind, self.workdir
        )
    }
}

const PREAMBLE: &str = "Environments: every environment tool takes `execute_in`, naming exactly one environment below; it is never defaulted. Paths are that environment's own real paths, and nothing is shared between environments. A relative path, or a missing `cwd`, starts from the environment's workdir; `cwd` applies to one call only.";

const SCRATCH_NOTE: &str = "`scratch` is a cache, not storage: its files persist across turns and conversations in this project until the whole scratch is evicted after 7 days unused. Repos under /repos are re-cloned after an eviction and uncommitted changes there are lost; processes never survive one.";

/// The head text: preamble, manifests, the scratch note. Falls back to one
/// line per environment when the full manifests would not fit the cap.
fn system(described: &[Described], scratch: bool) -> String {
    let compose = |lines: Vec<String>| {
        let mut parts = vec![PREAMBLE.to_owned()];
        if lines.is_empty() {
            parts.push("No environments are configured for this project yet; the user adds them in the project's settings.".to_owned());
        } else {
            parts.push(lines.join("\n"));
        }
        if scratch {
            parts.push(SCRATCH_NOTE.to_owned());
        }
        parts.join("\n\n")
    };
    let full = compose(described.iter().map(Described::full).collect());
    if full.len() <= plugin::run::SYSTEM_CAP {
        return full;
    }
    let mut brief = compose(described.iter().map(Described::brief).collect());
    brief.push_str("\n\n(Manifests were left out for length; `exec` with `uname -a` and `command -v` answers what they would have.)");
    if brief.len() <= plugin::run::SYSTEM_CAP {
        return brief;
    }
    // Still too long: a project with that many machines gets their names
    // only, cut to fit.
    let names = described
        .iter()
        .map(|d| format!("`{}`", d.name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut text = format!("{PREAMBLE}\n\nEnvironments: {names}");
    if text.len() > plugin::run::SYSTEM_CAP {
        let mut end = plugin::run::SYSTEM_CAP - 3;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    text
}

fn manifest() -> Manifest {
    Manifest {
        id: ID.to_owned(),
        name: "Environments".to_owned(),
        version: "1.0.0".to_owned(),
        description: "Files and commands: the project's scratch sandbox and the user's own machines running faber-agent.".to_owned(),
        capabilities: Default::default(),
        tools: tools::definitions(),
        config: plugin::manifest::ConfigSpec {
            version: config::VERSION,
            schema: config::schema(),
            default: config::default(),
        },
    }
}

#[async_trait]
impl Plugin for Environments {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn exports(&self) -> Vec<String> {
        vec![HANDLE.to_owned()]
    }

    async fn open(
        &self,
        ctx: &SessionCtx,
        phase: Phase,
        config: &Json,
    ) -> Result<Vec<Content>, String> {
        let config = EnvironmentsConfig::parse(config).map_err(|errors| {
            errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let (project, owner) = self.session(ctx).await?;
        let described = self.manifests(project, owner, &config).await;

        match phase {
            Phase::Fresh => Ok(vec![Content::text(system(
                &described,
                config.scratch.enabled,
            ))]),
            Phase::Added | Phase::Reopened => {
                let lines: Vec<String> = described.iter().map(Described::full).collect();
                let text = if lines.is_empty() {
                    "This project has no environments now.".to_owned()
                } else {
                    format!("This project's environments are now:\n{}", lines.join("\n"))
                };
                let mut text = format!("<environments>{text}</environments>");
                if text.len() > plugin::notice::NOTICE_CAP {
                    let names = described
                        .iter()
                        .map(|d| d.brief())
                        .collect::<Vec<_>>()
                        .join("\n");
                    text = format!(
                        "<environments>This project's environments are now:\n{names}</environments>"
                    );
                }
                if phase == Phase::Added {
                    text = format!("<environments>{PREAMBLE}</environments>\n{text}");
                }
                if let Err(error) = ctx.post(Message::user(text), Delivery::OnMessage) {
                    tracing::warn!(%error, "environments manifest notice refused");
                }
                Ok(Vec::new())
            }
        }
    }

    async fn handle_tool(&self, ctx: &SessionCtx, config: &Json, call: ToolCall) -> ToolResult {
        let config = match EnvironmentsConfig::parse(config) {
            Ok(config) => config,
            Err(_) => {
                return ToolResult::failed(
                    plugin::Failure::HandlerFailure,
                    "The environments config could not be read.",
                );
            }
        };
        let (project, owner) = match self.session(ctx).await {
            Ok(found) => found,
            Err(error) => return ToolResult::failed(plugin::Failure::HandlerFailure, error),
        };

        self.report_status(ctx, &config).await;
        self.report_exits(ctx).await;

        let name = call
            .input
            .get("execute_in")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let (bound, workdir, notes) = match self.resolve(project, owner, &config, name).await {
            Ok(found) => found,
            Err(Unavailable::Missing(text)) => return tools::bad_request("env_missing", text),
            Err(Unavailable::Unreachable(text)) => {
                return ToolResult::failed(
                    plugin::Failure::UpstreamFailure,
                    format!("env_unreachable: {text}"),
                );
            }
        };
        let place = tools::Place {
            bound: &bound,
            workdir: workdir.as_deref(),
        };

        let mut effects = tools::Effects::default();
        let mut result = match call.name.as_str() {
            "exec" => tools::exec(&place, &call.input, &mut effects).await,
            "process" => tools::process(&place, &call.input, &mut effects).await,
            "read" => tools::read(&place, &call.input).await,
            "patch" => tools::patch(&place, &call.input).await,
            other => tools::bad_request(
                "bad_request",
                format!("`{other}` is not an environments tool"),
            ),
        };

        if !effects.started.is_empty()
            && let Some(state) = self.sessions.lock().await.get_mut(ctx.session_id())
        {
            for id in effects.started {
                state.watching.push((Arc::downgrade(&bound), id));
            }
        }

        if !notes.is_empty() {
            let mut content = vec![Content::text(format!("note: {}", notes.join("\nnote: ")))];
            content.append(&mut result.content);
            result.content = content;
        }
        result
    }

    fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>> {
        config::validate(config)
    }

    fn migrate(&self, config: Json, from: u32) -> Result<Json, Vec<ConfigError>> {
        // Version 1 is the only one there has been, so nothing older exists
        // to bring forward.
        if from == config::VERSION {
            return Ok(config);
        }
        Err(vec![ConfigError::whole(format!(
            "config version {from} is not one this plugin ever wrote; it writes {}",
            config::VERSION
        ))])
    }

    fn on_changed(&self, old: &Json, new: &Json) -> Vec<Message> {
        match (
            EnvironmentsConfig::parse(old),
            EnvironmentsConfig::parse(new),
        ) {
            (Ok(old), Ok(new)) => config::changed_message(&old, &new).into_iter().collect(),
            _ => Vec::new(),
        }
    }

    fn on_removed(&self, _config: &Json) -> Vec<Message> {
        vec![Message::user(
            "<environments>The user turned off environments for this project: there are no environment tools any more, and calls to them fail.</environments>",
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_valid_and_its_tools_are_prefixed_within_the_cap() {
        let manifest = manifest();
        manifest.check().unwrap();
        let names: Vec<String> = manifest
            .tools
            .iter()
            .map(|tool| plugin::manifest::tool_name(&manifest.id, &tool.name))
            .collect();
        assert_eq!(
            names,
            [
                "environments__exec",
                "environments__process",
                "environments__read",
                "environments__patch"
            ]
        );
    }

    fn described(count: usize, tools: usize) -> Vec<Described> {
        (0..count)
            .map(|index| Described {
                name: format!("machine-{index}"),
                kind: "machine",
                workdir: format!("/home/user/projects/{index}"),
                probe: Some(Probe {
                    os: "Linux".into(),
                    arch: "x86_64".into(),
                    shell: "/bin/sh".into(),
                    tools: (0..tools)
                        .map(|t| (format!("tool{t}"), "1.2.3".into()))
                        .collect(),
                }),
                notes: vec!["sandboxing: none".into()],
            })
            .collect()
    }

    #[test]
    fn the_system_head_always_fits_the_cap() {
        let small = system(&described(2, 3), true);
        assert!(small.contains("tool2 1.2.3"));
        assert!(small.contains("not storage"));

        let medium = system(&described(20, 10), true);
        assert!(medium.len() <= plugin::run::SYSTEM_CAP);
        assert!(medium.contains("machine-19"));
        assert!(medium.contains("left out for length"));

        let huge = system(&described(400, 10), false);
        assert!(huge.len() <= plugin::run::SYSTEM_CAP);
    }
}
