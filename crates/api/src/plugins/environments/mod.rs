//! Environments: the built-in plugin that owns everything about files and
//! commands.
//!
//! A project's config is one ordered list: at most one `scratch` (the
//! session's own sandbox) and any number of `user` environments, each naming
//! one the project's owner registered with Faber. The agent names the
//! environment on every call, and paths are that environment's real paths;
//! there is no namespace shared across environments.
//!
//! - **Rules:** in every tool's description ([`tools::PREAMBLE`]), static.
//! - **System head:** `open(fresh)` lists the enabled environments in config
//!   order, a user environment with its OS, arch, shell and home from the
//!   probe cache, and the scratch note when the session starts with scratch.
//! - **Mid-session changes:** `on-changed` names what changed by stable id,
//!   turning an entry on or off included, with the scratch note when scratch
//!   is turned on; `open(reopened)` posts the current list as a notice.
//! - **Notices:** posted only during this plugin's own calls. Each call checks
//!   the environments with a standing connection (faber-agent, containers on
//!   agent hosts included) and posts any status change since it last reported
//!   in the session, withdrawing an offline notice that has not landed yet if
//!   the machine is back. SSH has no standing connection, so a failed SSH call
//!   says so in its own result. Process exits are reported when a call
//!   observes them. A disabled entry is never reported on.
//! - **Mounts:** other plugins mount live views into the session's scratch
//!   through `faber:environments/mounts` ([`mounts`]); the scratch line in
//!   the head lists them, and changes arrive as notices.

pub mod config;
pub mod mounts;
pub mod places;
pub mod runtime;
pub mod tools;

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use diesel::{
    ExpressionMethods, JoinOnDsl, NullableExpressionMethods, OptionalExtension, QueryDsl,
};
use diesel_async::RunQueryDsl;
use environment::ProcId;
use plugin::{
    ConfigError, Content, Delivery, Json, Manifest, Message, Pending, Phase, Plugin, SessionCtx,
    ToolCall, ToolResult,
};
use uuid::Uuid;

use self::config::{EnvironmentConfig, EnvironmentsConfig, Kind, scratch_note};
use self::mounts::SessionMounts;
use self::places::{Bound, Places, Unavailable};
use self::runtime::{Call, CallGuard, Mode, ScratchKey};
use crate::db::DbPool;
use crate::environments::Registered;
use crate::models::host::{HostProbe, Transport};
use crate::schema::{project, session, workspace};

pub const ID: &str = "environments";

/// The interface dependents import instead of calling these tools, and the
/// one through which plugins mount into scratch.
pub use plugin::environments::{HANDLE, MOUNTS};

/// How a status or exit notice lands: after the next tool result, or before
/// the user's next message if the model stops first.
const STATUS_DELIVERY: Delivery = Delivery::AfterToolOrMessage;

/// How long a session's cached state is kept with no call from it. Past
/// that it is looked up again; what is lost is a status notice that may be
/// repeated and exit notices for processes nobody has asked about since.
const SESSION_TTL: Duration = Duration::from_secs(6 * 60 * 60);

pub struct Environments {
    manifest: Manifest,
    places: Arc<Places>,
    mounts: Arc<SessionMounts>,
    db: DbPool,
    /// Per-session cache, keyed by session id: who owns the session's project,
    /// and what was last reported to it. Losing it costs a repeated status
    /// notice, never a wrong answer.
    sessions: tokio::sync::Mutex<HashMap<String, SessionState>>,
}

struct SessionState {
    project: Uuid,
    owner: Uuid,
    /// By config entry id, so a rename keeps what was said.
    reported: HashMap<String, Reported>,
    /// Which host each registered environment rides on, and whether that
    /// host keeps a standing connection to report on. `None` for an id that
    /// is not the owner's.
    hosts: HashMap<Uuid, Option<(Uuid, bool)>>,
    /// Processes started here, with the name the agent called their
    /// environment by when it started them.
    watching: Vec<(Weak<Bound>, ProcId, String)>,
    /// When a call from this session was last seen.
    seen: Instant,
}

/// Where one call runs, once resolved.
struct Resolved {
    bound: Arc<Bound>,
    /// The config entry's name: what every result calls the environment.
    name: String,
    /// The mode that runs a scratch call; `None` for a user environment.
    mode: Option<Mode>,
    /// Held while the call runs.
    _guard: CallGuard,
}

struct Reported {
    online: bool,
    offline_notice: Option<Pending>,
}

impl Environments {
    pub fn new(db: DbPool, places: Arc<Places>, mounts: Arc<SessionMounts>) -> Self {
        Environments {
            manifest: manifest(),
            places,
            mounts,
            db,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The session's project and its owner, cached.
    ///
    /// A project session lives in its owner's personal workspace, and
    /// everything this plugin reaches is the owner's. A session anywhere else
    /// is refused rather than lending the owner's environments to whoever
    /// else can drive it.
    async fn session(&self, ctx: &SessionCtx) -> Result<(Uuid, Uuid), String> {
        if let Some(state) = self.sessions.lock().await.get_mut(ctx.session_id()) {
            state.seen = Instant::now();
            return Ok((state.project, state.owner));
        }
        let id: Uuid = ctx
            .session_id()
            .parse()
            .map_err(|_| "this session id is not one Faber issued".to_owned())?;
        let mut conn = self.db.get().await.map_err(|error| error.to_string())?;
        let found: Option<(Uuid, Uuid, Option<Uuid>)> = session::table
            .inner_join(project::table.on(project::id.nullable().eq(session::project_id)))
            .inner_join(workspace::table.on(workspace::id.eq(session::workspace_id)))
            .filter(session::id.eq(id))
            .select((project::id, project::owner_id, workspace::user_id))
            .first(&mut conn)
            .await
            .optional()
            .map_err(|error| error.to_string())?;
        let (project, owner, holder) =
            found.ok_or_else(|| "this session is not in a project".to_owned())?;
        if holder != Some(owner) {
            return Err(
                "this session is not in the project owner's own workspace, so the project's environments are not reachable from it"
                    .to_owned(),
            );
        }
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, state| state.seen.elapsed() < SESSION_TTL);
        sessions.insert(
            ctx.session_id().to_owned(),
            SessionState {
                project,
                owner,
                reported: HashMap::new(),
                hosts: HashMap::new(),
                watching: Vec::new(),
                seen: Instant::now(),
            },
        );
        Ok((project, owner))
    }

    /// Drops what is cached for a deleted session, so a call still on its
    /// way looks the session up again and finds it gone.
    pub async fn forget(&self, session: &str) {
        self.sessions.lock().await.remove(session);
    }

    /// Every enabled environment as the agent is told about it, in config
    /// order, from the probe cache. Never a live call.
    async fn describe(
        &self,
        ctx: &SessionCtx,
        owner: Uuid,
        config: &EnvironmentsConfig,
    ) -> Vec<Described> {
        let ids: Vec<Uuid> = config
            .enabled()
            .filter_map(|entry| entry.environment_id)
            .collect();
        let found = self.places.registered(owner, &ids).await;
        let probes = self.places.probes(owner, &found).await;

        let mut described = Vec::new();
        for entry in config.enabled() {
            match entry.kind {
                Kind::Scratch => {
                    let runtime = self.places.scratch();
                    let mut notes = Vec::new();
                    let declarations = match ctx.session_id().parse::<Uuid>() {
                        Ok(session) => self.mounts.served(session).await,
                        Err(_) => Vec::new(),
                    };
                    if let Some(line) = mounts::head_mounts(&declarations) {
                        notes.push(line);
                    }
                    if let Some(why) = runtime.unavailable() {
                        notes.push(format!("{why}: calls to it fail"));
                    }
                    described.push(Described {
                        name: entry.name.clone(),
                        kind: Kind::Scratch,
                        state: State::Scratch(notes),
                    });
                }
                Kind::User => {
                    let registered = entry
                        .environment_id
                        .and_then(|id| found.iter().find(|registered| registered.id() == id));
                    let state = match registered {
                        None => State::Unregistered,
                        Some(registered) => match probes.get(&registered.id()) {
                            Some(probe) => State::Probed(Probe::from(probe)),
                            None => State::Unprobed,
                        },
                    };
                    described.push(Described {
                        name: entry.name.clone(),
                        kind: Kind::User,
                        state,
                    });
                }
            }
        }
        described
    }

    /// Which hosts this config's enabled user environments ride on, looked up
    /// once per session and environment.
    async fn hosts(&self, ctx: &SessionCtx, owner: Uuid, config: &EnvironmentsConfig) {
        let missing: Vec<Uuid> = {
            let sessions = self.sessions.lock().await;
            let Some(state) = sessions.get(ctx.session_id()) else {
                return;
            };
            config
                .enabled()
                .filter_map(|entry| entry.environment_id)
                .filter(|id| !state.hosts.contains_key(id))
                .collect()
        };
        if missing.is_empty() {
            return;
        }
        let found = self.places.registered(owner, &missing).await;
        let mut sessions = self.sessions.lock().await;
        let Some(state) = sessions.get_mut(ctx.session_id()) else {
            return;
        };
        for id in missing {
            let host = found.iter().find(|registered| registered.id() == id).map(
                |registered: &Registered| {
                    (
                        registered.host.id,
                        registered.transport() == Some(Transport::Agent),
                    )
                },
            );
            state.hosts.insert(id, host);
        }
    }

    /// Posts what changed in the status of environments with a standing
    /// connection since the last report in this session. Entries that are
    /// turned off are forgotten, so turning one back on starts from what the
    /// agent is then told: that it is there.
    async fn report_status(&self, ctx: &SessionCtx, owner: Uuid, config: &EnvironmentsConfig) {
        self.hosts(ctx, owner, config).await;
        let mut sessions = self.sessions.lock().await;
        let Some(state) = sessions.get_mut(ctx.session_id()) else {
            return;
        };
        state
            .reported
            .retain(|id, _| config.enabled().any(|entry| entry.id == *id));

        for entry in config.enabled() {
            let Some((host, true)) = entry
                .environment_id
                .and_then(|id| state.hosts.get(&id).copied().flatten())
            else {
                continue;
            };
            let online = self.places.online(host);
            let reported = state.reported.entry(entry.id.clone()).or_insert(Reported {
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
                post(ctx, format!("`{}` is back online.", entry.name));
            } else {
                reported.offline_notice = post(
                    ctx,
                    format!(
                        "`{}` is offline: its faber-agent is not connected, so calls to it fail until it reconnects.",
                        entry.name
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
        for (bound, id, name) in watching {
            let Some(live) = bound.upgrade() else {
                continue;
            };
            match live.target.processes().await {
                Ok(processes) => {
                    if let Some(process) = processes.iter().find(|process| process.id == id) {
                        match process.outcome {
                            Some(outcome) => {
                                post(
                                    ctx,
                                    format!(
                                        "The {}.",
                                        tools::exit_line(&name, &live.process_id(id), &outcome)
                                    ),
                                );
                            }
                            None => still.push((bound, id, name)),
                        }
                    }
                }
                Err(_) => still.push((bound, id, name)),
            }
        }

        if let Some(state) = self.sessions.lock().await.get_mut(ctx.session_id()) {
            state.watching.extend(still);
        }
    }

    /// Where a call runs, and for scratch the mode that will run it. A
    /// scratch call refused before it ran still says which mode scratch is
    /// in, when it has one.
    async fn resolve(
        &self,
        ctx: &SessionCtx,
        project: Uuid,
        owner: Uuid,
        config: &EnvironmentsConfig,
        name: &str,
        call: Call<'_>,
    ) -> Result<Resolved, (Unavailable, Option<Mode>)> {
        let entry: &EnvironmentConfig = match config.named(name) {
            Some(entry) if entry.enabled => entry,
            Some(_) => {
                return Err((
                    Unavailable::Missing(format!("`{name}` is turned off for this project")),
                    None,
                ));
            }
            None => {
                let names = config.names();
                return Err((
                    Unavailable::Missing(format!(
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
                    )),
                    None,
                ));
            }
        };
        match entry.kind {
            Kind::Scratch => {
                let key = ScratchKey {
                    session: ctx.session_id().to_owned(),
                    owner,
                };
                let runtime = self.places.scratch();
                let routed = match runtime.route(&key, call).await {
                    Ok(routed) => routed,
                    Err(why) => return Err((why, runtime.mode(&key).await)),
                };
                if routed.opened
                    && let Ok(session) = ctx.session_id().parse::<Uuid>()
                    && let Some(gone) = self.mounts.restore(session).await
                    && let Err(error) = ctx.post(gone, STATUS_DELIVERY)
                {
                    tracing::warn!(%error, "environments notice refused");
                }
                Ok(Resolved {
                    bound: routed.bound,
                    name: entry.name.clone(),
                    mode: Some(routed.mode),
                    _guard: routed.guard,
                })
            }
            Kind::User => match self.places.user(project, owner, entry).await {
                Ok(bound) => Ok(Resolved {
                    bound,
                    name: entry.name.clone(),
                    mode: None,
                    _guard: CallGuard::none(),
                }),
                Err(why) => Err((why, None)),
            },
        }
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
    home: String,
}

impl From<&HostProbe> for Probe {
    fn from(probe: &HostProbe) -> Self {
        Probe {
            os: probe.os.clone().unwrap_or_else(|| "unknown os".into()),
            arch: probe.arch.clone().unwrap_or_else(|| "unknown arch".into()),
            shell: probe.shell.clone().unwrap_or_else(|| "/bin/sh".into()),
            // A probe from before `home` was asked: its root is still a
            // place every path may start from.
            home: probe
                .home
                .clone()
                .or_else(|| probe.root_path.clone())
                .unwrap_or_else(|| "/".into()),
        }
    }
}

/// What is known about one environment for the head.
enum State {
    /// Scratch, with its notes (mounts, availability).
    Scratch(Vec<String>),
    Probed(Probe),
    /// Registered but never probed: the first call probes it.
    Unprobed,
    /// Names nothing the project's owner has registered.
    Unregistered,
}

/// One environment as the agent is told about it.
struct Described {
    name: String,
    kind: Kind,
    state: State,
}

impl Described {
    fn full(&self) -> String {
        let mut line = self.brief();
        match &self.state {
            State::Scratch(notes) => {
                for note in notes {
                    line.push_str("; ");
                    line.push_str(note);
                }
            }
            State::Probed(probe) => line.push_str(&format!(
                ": {} {}, shell {}, home {}",
                probe.os, probe.arch, probe.shell, probe.home
            )),
            State::Unprobed => line.push_str(": not probed yet; the first call probes it"),
            State::Unregistered => line.push_str(
                ": not one of the project owner's registered environments, so calls to it fail",
            ),
        }
        line
    }

    fn brief(&self) -> String {
        format!("- `{}` ({})", self.name, self.kind.as_str())
    }
}

/// The head text: the environment list and, when the session starts with
/// scratch, its note. Falls back to one short line per environment, then to
/// names cut to fit, when the full list would not fit the cap.
fn system(described: &[Described], scratch: Option<&str>) -> String {
    let note = scratch.map(scratch_note);
    let compose = |lines: Vec<String>| {
        let mut parts = Vec::new();
        if lines.is_empty() {
            parts.push("No environments are turned on for this project; the user adds them in the project's settings.".to_owned());
        } else {
            parts.push(format!("Environments:\n{}", lines.join("\n")));
        }
        if let Some(note) = &note {
            parts.push(note.clone());
        }
        parts.join("\n\n")
    };
    let full = compose(described.iter().map(Described::full).collect());
    if full.len() <= plugin::run::SYSTEM_CAP {
        return full;
    }
    let mut brief = compose(described.iter().map(Described::brief).collect());
    brief.push_str("\n\n(Details were left out for length; `exec` with `uname -a` and `echo $HOME` answers what they would have.)");
    if brief.len() <= plugin::run::SYSTEM_CAP {
        return brief;
    }
    // Still too long: a project with that many environments gets their names
    // only, cut to fit.
    let names = described
        .iter()
        .map(|d| format!("`{}`", d.name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut text = format!("Environments: {names}");
    if let Some(note) = &note {
        text = format!("{note}\n\n{text}");
    }
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
        version: "2.0.0".to_owned(),
        description: "Files and commands: the session's scratch sandbox and the user's own registered environments.".to_owned(),
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
        vec![HANDLE.to_owned(), MOUNTS.to_owned()]
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
        let (_project, owner) = self.session(ctx).await?;
        let described = self.describe(ctx, owner, &config).await;
        let scratch = config.scratch_name();

        match phase {
            Phase::Fresh => Ok(vec![Content::text(system(&described, scratch))]),
            Phase::Added | Phase::Reopened => {
                let lines: Vec<String> = described.iter().map(Described::full).collect();
                let list = if lines.is_empty() {
                    "This project has no environments turned on now.".to_owned()
                } else {
                    format!("This project's environments are now:\n{}", lines.join("\n"))
                };
                let note = match scratch {
                    // Joining mid-session is the session's first sight of
                    // scratch; a reopen already heard about it on change.
                    Some(name) if phase == Phase::Added => format!("\n\n{}", scratch_note(name)),
                    _ => String::new(),
                };
                let mut text = format!("<environments>{list}{note}</environments>");
                if text.len() > plugin::notice::NOTICE_CAP {
                    let names = described
                        .iter()
                        .map(|d| d.brief())
                        .collect::<Vec<_>>()
                        .join("\n");
                    text = format!(
                        "<environments>This project's environments are now:\n{names}{note}</environments>"
                    );
                }
                if let Err(error) = ctx.post(Message::user(text), Delivery::OnMessage) {
                    tracing::warn!(%error, "environments list notice refused");
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

        self.report_status(ctx, owner, &config).await;
        self.report_exits(ctx).await;

        // A call refused for its shape is refused before it is routed, so
        // it never opens or promotes scratch.
        if let Err(refused) = tools::precheck(&call.name, &call.input) {
            return refused;
        }
        let name = call
            .input
            .get("execute_in")
            .and_then(Json::as_str)
            .unwrap_or_default();
        let kind = tools::call_kind(&call.name, &call.input);
        let resolved = match self.resolve(ctx, project, owner, &config, name, kind).await {
            Ok(resolved) => resolved,
            Err((Unavailable::Missing(text), mode)) => {
                return tools::with_mode(tools::bad_request("env_missing", text), mode);
            }
            Err((Unavailable::Unreachable(text), mode)) => {
                return tools::with_mode(
                    ToolResult::failed(
                        plugin::Failure::UpstreamFailure,
                        format!("env_unreachable: {text}"),
                    ),
                    mode,
                );
            }
        };
        let place = tools::Place {
            bound: &resolved.bound,
            name: &resolved.name,
        };

        let mut effects = tools::Effects::default();
        let result = tools::dispatch(&place, &call.name, &call.input, &mut effects).await;

        if resolved.mode.is_some() && !effects.managed.is_empty() {
            let key = ScratchKey {
                session: ctx.session_id().to_owned(),
                owner,
            };
            self.places.scratch().manage(&key, &effects.managed).await;
        }
        if !effects.started.is_empty()
            && let Some(state) = self.sessions.lock().await.get_mut(ctx.session_id())
        {
            for id in effects.started {
                state
                    .watching
                    .push((Arc::downgrade(&resolved.bound), id, resolved.name.clone()));
            }
        }

        // The mode settled before the call is the one that ran it: nothing
        // that ran virtually is run again in compute.
        tools::with_mode(result, resolved.mode)
    }

    fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>> {
        config::validate(config)
    }

    fn migrate(&self, config: Json, from: u32) -> Result<Json, Vec<ConfigError>> {
        migrate(config, from)
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

/// Brings a stored config to [`config::VERSION`].
///
/// Version 1 (scratch settings and a list of faber-agent machines with
/// workdirs) never reached a deployment that mattered, and none of its parts
/// map onto a list of registered environments, so it becomes the default:
/// scratch alone. The user re-adds their environments by id.
fn migrate(config: Json, from: u32) -> Result<Json, Vec<ConfigError>> {
    match from {
        config::VERSION => Ok(config),
        1 => Ok(config::default()),
        _ => Err(vec![ConfigError::whole(format!(
            "config version {from} is not one this plugin ever wrote; it writes {}",
            config::VERSION
        ))]),
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

    fn described(count: usize) -> Vec<Described> {
        (0..count)
            .map(|index| Described {
                name: format!("machine-{index}"),
                kind: Kind::User,
                state: State::Probed(Probe {
                    os: "linux".into(),
                    arch: "x86_64".into(),
                    shell: "/bin/sh".into(),
                    home: format!("/home/user-with-a-long-name/{index}"),
                }),
            })
            .collect()
    }

    #[test]
    fn a_user_line_names_type_os_arch_shell_and_home() {
        let lines = described(1);
        assert_eq!(
            lines[0].full(),
            "- `machine-0` (user): linux x86_64, shell /bin/sh, home /home/user-with-a-long-name/0"
        );
        let scratch = Described {
            name: "scratch".into(),
            kind: Kind::Scratch,
            state: State::Scratch(vec!["mounts: /mnt/notes/inbox".into()]),
        };
        assert_eq!(
            scratch.full(),
            "- `scratch` (scratch); mounts: /mnt/notes/inbox"
        );
    }

    #[test]
    fn the_scratch_note_is_in_the_head_only_when_scratch_is() {
        const NOTE: &str = "`scratch` belongs to this session. Its files are kept while the session lives; mounts under /mnt are live views, not copies.";
        assert!(system(&described(1), Some("scratch")).ends_with(NOTE));
        assert!(!system(&described(1), None).contains("belongs to this session"));
        assert!(
            system(&described(1), Some("sandbox")).contains("`sandbox` belongs to this session.")
        );
        assert_eq!(
            system(&[], None),
            "No environments are turned on for this project; the user adds them in the project's settings."
        );
    }

    #[test]
    fn the_system_head_always_fits_the_cap() {
        let small = system(&described(2), Some("scratch"));
        assert!(small.contains("home /home/user-with-a-long-name/1"));

        let medium = system(&described(60), Some("scratch"));
        assert!(medium.len() <= plugin::run::SYSTEM_CAP);
        assert!(medium.contains("machine-59"));
        assert!(medium.contains("left out for length"));
        assert!(medium.contains(&scratch_note("scratch")));

        let huge = system(&described(400), Some("scratch"));
        assert!(huge.len() <= plugin::run::SYSTEM_CAP);
    }

    #[test]
    fn version_one_becomes_the_default_and_nothing_else_unknown_is_taken() {
        let old =
            serde_json::json!({ "scratch": { "enabled": true, "repos": [] }, "machines": [] });
        assert_eq!(migrate(old, 1).unwrap(), config::default());
        assert!(migrate(config::default(), 3).is_err());
        assert!(migrate(config::default(), 0).is_err());
        assert_eq!(migrate(config::default(), 2).unwrap(), config::default());
    }
}
