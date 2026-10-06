//! Run start: comparing the snapshot a session last ran with to the project's
//! bindings now, running the lifecycle that difference calls for, and handing
//! the run its system head, its tools and a dispatcher for them.
//!
//! For a changed binding the order is `migrate`, `validate`,
//! `on-changed(old, new)`, then `open(reopened)`; hook messages are queued
//! before anything `open` posts. A removed or disabled binding runs
//! `on-removed` on the last version it was bound at. `open` runs in
//! dependency order; everything else — head, tools, notices — in creation
//! order.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bind::{self, Binding};
use crate::manifest::{self, ToolDefinition};
use crate::notice::{NOTICE_CAP, Notices};
use crate::plugin::{Plugin, SessionCtx};
use crate::registry::Registry;
use crate::schema;
use crate::types::{
    Delivery, Failure, Json, Message, Phase, ToolCall, ToolResult, joined, text_len,
};

/// Bytes of `system` text one binding may put in the head.
pub const SYSTEM_CAP: usize = 4 * 1024;
/// Messages one hook call may return.
pub const HOOK_MESSAGES_CAP: usize = 64;

/// Why a binding is degraded for a session snapshot. Goes to logs and the UI,
/// never to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Degraded {
    OpenFailed,
    SystemTooLarge,
    DependencyMissing,
}

impl Degraded {
    fn failure(self) -> Failure {
        match self {
            Degraded::DependencyMissing => Failure::UpstreamFailure,
            Degraded::OpenFailed | Degraded::SystemTooLarge => Failure::HandlerFailure,
        }
    }

    /// What the model reads instead of a result: that the tool cannot work in
    /// this session, never why in the core's terms.
    fn content(self, tool: &str) -> String {
        match self {
            Degraded::DependencyMissing => format!(
                "`{tool}` is unavailable in this session: something it depends on is not available. \
                 The user can see why in the project's plugin settings."
            ),
            Degraded::OpenFailed | Degraded::SystemTooLarge => format!(
                "`{tool}` is unavailable in this session: its plugin could not start. \
                 The user can see why in the project's plugin settings."
            ),
        }
    }
}

/// What a session last ran with, one entry per enabled binding. Stored with
/// the session and compared at the next run start.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub bindings: Vec<SnapshotEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub plugin: String,
    pub version: String,
    /// Normalized: after `migrate` and `validate`.
    pub config: Json,
    pub order: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded: Option<Degraded>,
}

impl Snapshot {
    fn entry(&self, plugin: &str) -> Option<&SnapshotEntry> {
        self.bindings.iter().find(|entry| entry.plugin == plugin)
    }
}

/// What run start did with one binding, for logs and the UI.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BindingReport {
    pub plugin: String,
    /// `None` when the binding was unchanged and nothing was opened.
    pub phase: Option<Phase>,
    pub degraded: Option<Degraded>,
    /// The `open` or config error string, when there was one.
    pub detail: Option<String>,
}

/// Everything run start hands a run.
pub struct Started {
    /// The composed head, on a session's first run only. Never edited after.
    pub head: Option<String>,
    /// What to store as the session's snapshot once the run commits.
    pub snapshot: Snapshot,
    /// The run's tool array: bindings in creation order, each type's tools in
    /// declared order, every name prefixed.
    pub tools: Vec<llm::ToolDef>,
    pub dispatcher: Dispatcher,
    pub report: Vec<BindingReport>,
}

/// One enabled binding being started.
struct Slot {
    binding: Binding,
    plugin: Option<Arc<dyn Plugin>>,
    config: Json,
    phase: Option<Phase>,
    degraded: Option<Degraded>,
    detail: Option<String>,
    system: Option<String>,
}

/// Runs the lifecycle for one run start. `previous` is the snapshot the
/// session's last run used; `None` is its first run.
pub async fn start(
    registry: &Registry,
    session_id: &str,
    bindings: &[Binding],
    previous: Option<&Snapshot>,
    notices: &Arc<Notices>,
    core_prompt: &str,
) -> Started {
    let mut bindings: Vec<Binding> = bindings.to_vec();
    bindings.sort_by(|a, b| (a.order, &a.plugin).cmp(&(b.order, &b.plugin)));
    let enabled: Vec<Binding> = bindings.into_iter().filter(|b| b.enabled).collect();

    // Removed or disabled since the last run: the last bound version speaks.
    if let Some(previous) = previous {
        for gone in previous
            .bindings
            .iter()
            .filter(|entry| !enabled.iter().any(|b| b.plugin == entry.plugin))
        {
            let Some(plugin) = registry
                .get(&gone.plugin, &gone.version)
                .or_else(|| registry.latest(&gone.plugin))
            else {
                continue;
            };
            let messages = plugin.on_removed(&gone.config);
            queue_hooks(notices, &gone.plugin, gone.order, messages);
        }
    }

    // Resolve, normalize and decide a phase for each enabled binding.
    let mut slots: Vec<Slot> = Vec::with_capacity(enabled.len());
    for binding in enabled {
        let plugin = registry.get(&binding.plugin, &binding.version);
        let before = previous.and_then(|previous| previous.entry(&binding.plugin));

        let mut slot = Slot {
            config: binding.config.clone(),
            plugin: plugin.clone(),
            binding,
            phase: None,
            degraded: None,
            detail: None,
            system: None,
        };

        let Some(plugin) = plugin else {
            slot.degraded = Some(Degraded::OpenFailed);
            slot.detail = Some(format!(
                "`{}` {} is not registered on this host",
                slot.binding.plugin, slot.binding.version
            ));
            slot.phase = Some(phase_for(previous, before));
            slots.push(slot);
            continue;
        };

        match bind::normalize(
            plugin.as_ref(),
            slot.binding.config.clone(),
            slot.binding.config_version,
        ) {
            Ok(config) => slot.config = config,
            Err(errors) => {
                // A config that passed at bind time and fails now (an upgrade
                // whose migrate refuses it): the binding cannot open.
                slot.degraded = Some(Degraded::OpenFailed);
                slot.detail = Some(format!(
                    "config: {}",
                    errors
                        .iter()
                        .map(|error| format!("{} {}", error.path, error.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                ));
                slot.phase = Some(phase_for(previous, before));
                slots.push(slot);
                continue;
            }
        }

        slot.phase = match (previous, before) {
            (None, _) => Some(Phase::Fresh),
            (Some(_), None) => Some(Phase::Added),
            (Some(_), Some(before)) => {
                let changed =
                    before.version != slot.binding.version || before.config != slot.config;
                if changed {
                    let messages = plugin.on_changed(&before.config, &slot.config);
                    queue_hooks(notices, &slot.binding.plugin, slot.binding.order, messages);
                    Some(Phase::Reopened)
                } else if before.degraded.is_some() {
                    // A failed open is retried at every run start.
                    Some(Phase::Reopened)
                } else {
                    None
                }
            }
        };
        slots.push(slot);
    }

    // `open`, in dependency order.
    let snapshot_bindings: Vec<Binding> = slots.iter().map(|slot| slot.binding.clone()).collect();
    let providers = bind::providers(registry, &snapshot_bindings).unwrap_or_default();
    let edges = bind::edges(registry, &snapshot_bindings, &providers);
    for index in dependency_order(&slots, &edges) {
        // Imports are required: a missing, disabled or degraded provider
        // degrades the importer, and it is not opened.
        let missing = slots[index].plugin.as_ref().is_some_and(|plugin| {
            plugin
                .imports()
                .iter()
                .any(|interface| match providers.get(interface) {
                    None => true,
                    Some(provider) => slots
                        .iter()
                        .find(|slot| &slot.binding.plugin == provider)
                        .is_none_or(|slot| slot.degraded.is_some()),
                })
        });
        let slot = &mut slots[index];
        if slot.degraded.is_some() {
            continue;
        }
        if missing {
            slot.degraded = Some(Degraded::DependencyMissing);
            continue;
        }
        let (Some(plugin), Some(phase)) = (slot.plugin.clone(), slot.phase) else {
            continue;
        };

        let ctx = SessionCtx::new(
            session_id,
            slot.binding.plugin.clone(),
            slot.binding.order,
            Arc::clone(notices),
        );
        match plugin.open(&ctx, phase, &slot.config).await {
            Ok(system) => {
                if phase == Phase::Fresh {
                    if text_len(&system) > SYSTEM_CAP {
                        slot.degraded = Some(Degraded::SystemTooLarge);
                        slot.detail = Some(format!(
                            "system is {} bytes; the limit is {SYSTEM_CAP}",
                            text_len(&system)
                        ));
                    } else if !system.is_empty() {
                        slot.system = Some(joined(&system));
                    }
                }
            }
            Err(error) => {
                slot.degraded = Some(Degraded::OpenFailed);
                slot.detail = Some(error);
            }
        }
    }

    for slot in slots.iter().filter(|slot| slot.degraded.is_some()) {
        tracing::warn!(
            plugin = %slot.binding.plugin,
            reason = ?slot.degraded,
            detail = slot.detail.as_deref().unwrap_or(""),
            "binding degraded for this session snapshot"
        );
    }

    // The head: composed once, on the first run, in creation order.
    let head = previous.is_none().then(|| {
        std::iter::once(core_prompt.to_owned())
            .chain(slots.iter().filter_map(|slot| slot.system.clone()))
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    });

    let mut tools = Vec::new();
    let mut routes = HashMap::new();
    for slot in &slots {
        let Some(plugin) = &slot.plugin else { continue };
        for tool in &plugin.manifest().tools {
            let sent = manifest::tool_name(&slot.binding.plugin, &tool.name);
            tools.push(llm::ToolDef {
                name: sent.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.clone(),
            });
            routes.insert(
                sent,
                Route {
                    plugin: Arc::clone(plugin),
                    binding: slot.binding.plugin.clone(),
                    order: slot.binding.order,
                    config: slot.config.clone(),
                    tool: tool.clone(),
                    degraded: slot.degraded,
                },
            );
        }
    }

    let snapshot = Snapshot {
        bindings: slots
            .iter()
            .map(|slot| SnapshotEntry {
                plugin: slot.binding.plugin.clone(),
                version: slot.binding.version.clone(),
                config: slot.config.clone(),
                order: slot.binding.order,
                degraded: slot.degraded,
            })
            .collect(),
    };

    let report = slots
        .into_iter()
        .map(|slot| BindingReport {
            plugin: slot.binding.plugin,
            phase: slot.phase,
            degraded: slot.degraded,
            detail: slot.detail,
        })
        .collect();

    Started {
        head,
        snapshot,
        tools,
        dispatcher: Dispatcher {
            routes: Arc::new(routes),
            notices: Arc::clone(notices),
            session_id: session_id.to_owned(),
            calls: Arc::new(AtomicU64::new(0)),
        },
        report,
    }
}

fn phase_for(previous: Option<&Snapshot>, before: Option<&SnapshotEntry>) -> Phase {
    match (previous, before) {
        (None, _) => Phase::Fresh,
        (Some(_), None) => Phase::Added,
        (Some(_), Some(_)) => Phase::Reopened,
    }
}

fn queue_hooks(notices: &Notices, plugin: &str, order: i64, messages: Vec<Message>) {
    if messages.len() > HOOK_MESSAGES_CAP {
        tracing::warn!(%plugin, count = messages.len(), "hook returned too many messages; the rest are dropped");
    }
    for message in messages.into_iter().take(HOOK_MESSAGES_CAP) {
        if let Err(error) = notices.post_hook(plugin, order, message) {
            tracing::warn!(%plugin, %error, "hook message refused");
        }
    }
}

/// Slot indices with every provider before its importers; creation order
/// otherwise. Cycles were refused at bind time, so whatever remains in one is
/// appended in creation order.
fn dependency_order(slots: &[Slot], edges: &BTreeMap<String, Vec<String>>) -> Vec<usize> {
    let mut order = Vec::with_capacity(slots.len());
    let mut placed: BTreeSet<usize> = BTreeSet::new();
    while placed.len() < slots.len() {
        let before = placed.len();
        for (index, slot) in slots.iter().enumerate() {
            if placed.contains(&index) {
                continue;
            }
            let ready = edges
                .get(&slot.binding.plugin)
                .into_iter()
                .flatten()
                .all(|provider| {
                    slots
                        .iter()
                        .position(|other| &other.binding.plugin == provider)
                        .is_none_or(|position| placed.contains(&position) || position == index)
                });
            if ready {
                placed.insert(index);
                order.push(index);
            }
        }
        if placed.len() == before {
            for index in 0..slots.len() {
                if placed.insert(index) {
                    order.push(index);
                }
            }
        }
    }
    order
}

struct Route {
    plugin: Arc<dyn Plugin>,
    binding: String,
    order: i64,
    config: Json,
    tool: ToolDefinition,
    degraded: Option<Degraded>,
}

/// Routes a run's tool calls to the bindings that declared them.
///
/// Every call ends in a [`ToolResult`]: an unknown tool and a schema failure
/// are `bad-request`s the core writes, a degraded binding is answered by the
/// core, and a plugin that panics is a `handler-failure`. There is no
/// separate dispatch-failure channel.
#[derive(Clone)]
pub struct Dispatcher {
    routes: Arc<HashMap<String, Route>>,
    notices: Arc<Notices>,
    session_id: String,
    calls: Arc<AtomicU64>,
}

impl Dispatcher {
    /// Runs one call. The plugin's half runs on its own task: dropping the
    /// returned future (an interrupt) detaches the call rather than cancelling
    /// it, and a detached call's result is posted to the session as an
    /// `on-message` `system` notice when it finishes.
    pub async fn invoke(&self, name: &str, input: Value, id: Option<String>) -> ToolResult {
        let Some(route) = self.routes.get(name) else {
            return ToolResult::failed(
                Failure::BadRequest,
                format!("`{name}` is not a tool in this session."),
            );
        };

        if let Some(degraded) = route.degraded {
            return ToolResult::failed(degraded.failure(), degraded.content(name));
        }

        let violations = schema::check(&route.tool.input_schema, &input);
        if !violations.is_empty() {
            return ToolResult::failed(
                Failure::BadRequest,
                format!(
                    "`{name}` was called with input that does not fit its schema:\n{}",
                    violations
                        .iter()
                        .map(|violation| format!("- {violation}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            );
        }

        let call = ToolCall {
            id: id
                .unwrap_or_else(|| format!("call_{}", self.calls.fetch_add(1, Ordering::Relaxed))),
            name: route.tool.name.clone(),
            input,
        };
        let ctx = SessionCtx::new(
            self.session_id.clone(),
            route.binding.clone(),
            route.order,
            Arc::clone(&self.notices),
        );
        let plugin = Arc::clone(&route.plugin);
        let config = route.config.clone();
        let notices = Arc::clone(&self.notices);
        let binding = route.binding.clone();
        let order = route.order;
        let sent = name.to_owned();

        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let handled = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                plugin.handle_tool(&ctx, &config, call),
            ))
            .await;
            let Ok(result) = handled else {
                // A trap: nothing to report late, and the caller (if still
                // waiting) gets a handler-failure.
                let _ = tx.send(None);
                return;
            };
            if let Err(Some(result)) = tx.send(Some(result)) {
                let text = cut(
                    &format!(
                        "A `{sent}` call that was interrupted has since finished. Its result:\n{}",
                        joined(&result.content)
                    ),
                    NOTICE_CAP,
                );
                if let Err(error) =
                    notices.post(&binding, order, Message::system(text), Delivery::OnMessage)
                {
                    tracing::warn!(%binding, %error, "late result could not be posted");
                }
            }
        });

        match rx.await {
            Ok(Some(result)) => result,
            Ok(None) | Err(_) => ToolResult::failed(
                Failure::HandlerFailure,
                format!("`{name}` failed inside its plugin; the call's outcome is unknown."),
            ),
        }
    }
}

/// Cuts text to `cap` bytes on a character boundary.
fn cut(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let marker = "\n[cut]";
    let mut end = cap - marker.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
}
