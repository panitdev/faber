//! The daemon supervisor: one daemon instance per plugin version with at
//! least one enabled binding, never one per binding, started, fed binding
//! events, restarted with backoff and degraded like a service under systemd.
//!
//! The supervisor holds the set of bindings each instance should serve and
//! diffs it against what the database says ([`Supervisor::reconcile`]),
//! which the routes call after every binding change. Each instance is an
//! actor task:
//!
//! - **start**: a fresh `serve` call gets one `added` per current binding,
//!   each with a new handle, before anything else;
//! - **changes**: a new config is `changed` with a new handle (the old one's
//!   lease is retired, so it answers `gone`); a disabled, unbound or
//!   upgraded binding is `removed`, its lease retired and its sockets closed;
//! - **failure**: `serve` returning or panicking while bindings remain is a
//!   failure, restarted after a backoff that doubles from `backoff_min` to
//!   `backoff_max`; `max_failures` within `failure_window` degrades the
//!   instance until a binding changes or an operator retries. Degraded
//!   bindings keep serving tools: nothing here touches them;
//! - **stop**: when its last binding goes, or at shutdown, the event stream
//!   closes and `serve` gets `grace` to return before it is dropped.
//!
//! Instances run on the runtime the supervisor was made on — the server's —
//! never on a run's. Faber is one process, so "one instance" needs no
//! election; a second API process would need one per plugin.
//!
//! Built-ins run natively, so the memory and wake-time caps of the draft
//! contract are not enforced here; a component host will.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plugin::daemon::{Event, EventSender, Events, GRACE, SharedBinding, SharedDaemon};
use plugin::{Json, Registry};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::websocket::PluginSockets;
use super::{BindingKey, Lease};

/// The supervisor's timings and failure budget.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub grace: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    pub max_failures: usize,
    pub failure_window: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            grace: GRACE,
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(5 * 60),
            max_failures: 10,
            failure_window: Duration::from_secs(60 * 60),
        }
    }
}

/// Makes the handle a daemon gets for one binding. The host's is
/// [`super::sessions::Sessions`].
pub trait Handles: Send + Sync + 'static {
    fn handle(&self, binding: BindingKey, config: Json, lease: Lease) -> SharedBinding;
}

impl Handles for Arc<super::sessions::Sessions> {
    fn handle(&self, binding: BindingKey, config: Json, lease: Lease) -> SharedBinding {
        Arc::new(super::sessions::Sessions::handle(
            self, binding, config, lease,
        ))
    }
}

/// One binding an instance should serve.
#[derive(Clone, Debug, PartialEq)]
pub struct Desired {
    pub binding: BindingKey,
    /// The plugin version bound.
    pub version: String,
    /// Normalized: migrated and validated, as at bind time.
    pub config: Json,
}

/// What an instance is doing, for logs and each bound project's UI. An
/// error here is never shown to a model.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Status {
    Running,
    /// Failed; restarting after a backoff.
    Restarting {
        error: String,
        failures: usize,
    },
    /// Failed too often; not restarted until a binding changes or an
    /// operator retries.
    Degraded {
        error: String,
        failures: usize,
    },
}

/// (plugin id, version): what one instance serves.
type Version = (String, String);

enum Control {
    /// Serve this binding with this config: `added` if new, `changed` if
    /// the config differs, nothing if it is the same.
    Upsert(BindingKey, Json),
    Remove(BindingKey),
    Retry,
    Stop,
}

struct Instance {
    control: mpsc::UnboundedSender<Control>,
    serving: BTreeMap<BindingKey, Json>,
    task: JoinHandle<()>,
}

pub struct Supervisor {
    registry: Arc<Registry>,
    handles: Arc<dyn Handles>,
    limits: Limits,
    runtime: tokio::runtime::Handle,
    sockets: Mutex<HashMap<String, Arc<PluginSockets>>>,
    instances: tokio::sync::Mutex<HashMap<Version, Instance>>,
    status: Arc<Mutex<HashMap<Version, Status>>>,
}

impl Supervisor {
    /// Instances run on the runtime this is called on.
    pub fn new(registry: Arc<Registry>, handles: Arc<dyn Handles>, limits: Limits) -> Self {
        Supervisor {
            registry,
            handles,
            limits,
            runtime: tokio::runtime::Handle::current(),
            sockets: Mutex::new(HashMap::new()),
            instances: tokio::sync::Mutex::new(HashMap::new()),
            status: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The sockets a plugin's daemon connects through, so the supervisor can
    /// close a binding's when it is removed.
    pub fn sockets(&self, plugin: &str, sockets: Arc<PluginSockets>) {
        self.sockets
            .lock()
            .expect("supervisor sockets poisoned")
            .insert(plugin.to_owned(), sockets);
    }

    /// The instance serving `plugin` at `version`, if one is.
    pub fn status(&self, plugin: &str, version: &str) -> Option<Status> {
        self.status
            .lock()
            .expect("supervisor status poisoned")
            .get(&(plugin.to_owned(), version.to_owned()))
            .cloned()
    }

    /// Brings every instance in line with `desired`: the bindings that
    /// should be served now. Starts instances for versions that gained their
    /// first binding and stops those that lost their last.
    pub async fn reconcile(&self, desired: Vec<Desired>) {
        let mut wanted: HashMap<Version, BTreeMap<BindingKey, Json>> = HashMap::new();
        for row in desired {
            if self.daemon(&row.binding.plugin, &row.version).is_none() {
                continue;
            }
            wanted
                .entry((row.binding.plugin.clone(), row.version))
                .or_default()
                .insert(row.binding, row.config);
        }

        let mut instances = self.instances.lock().await;
        // Removals first, so an upgrade's `removed` reaches the old version
        // before the new version's `added`.
        let mut emptied = Vec::new();
        for (version, instance) in instances.iter_mut() {
            let target = wanted.get(version);
            let gone: Vec<BindingKey> = instance
                .serving
                .keys()
                .filter(|key| target.is_none_or(|target| !target.contains_key(*key)))
                .cloned()
                .collect();
            for key in gone {
                instance.serving.remove(&key);
                let _ = instance.control.send(Control::Remove(key));
            }
            if instance.serving.is_empty() {
                emptied.push(version.clone());
            }
        }
        for version in emptied {
            if let Some(instance) = instances.remove(&version) {
                let _ = instance.control.send(Control::Stop);
            }
            self.status
                .lock()
                .expect("supervisor status poisoned")
                .remove(&version);
        }

        for (version, target) in wanted {
            let alive = instances
                .get(&version)
                .is_some_and(|instance| !instance.task.is_finished());
            if !alive {
                instances.remove(&version);
                let Some(daemon) = self.daemon(&version.0, &version.1) else {
                    continue;
                };
                let instance = self.start(version.clone(), daemon, target);
                instances.insert(version, instance);
                continue;
            }
            let instance = instances.get_mut(&version).expect("checked above");
            for (key, config) in target {
                if instance.serving.get(&key) != Some(&config) {
                    instance.serving.insert(key.clone(), config.clone());
                    let _ = instance.control.send(Control::Upsert(key, config));
                }
            }
        }
    }

    /// An operator's retry of a degraded instance.
    pub async fn retry(&self, plugin: &str, version: &str) {
        let instances = self.instances.lock().await;
        if let Some(instance) = instances.get(&(plugin.to_owned(), version.to_owned())) {
            let _ = instance.control.send(Control::Retry);
        }
    }

    /// Stops every instance, each within its grace period.
    pub async fn shutdown(&self) {
        let stopping: Vec<Instance> = self
            .instances
            .lock()
            .await
            .drain()
            .map(|(_, i)| i)
            .collect();
        for instance in &stopping {
            let _ = instance.control.send(Control::Stop);
        }
        for instance in stopping {
            let _ = instance.task.await;
        }
        self.status
            .lock()
            .expect("supervisor status poisoned")
            .clear();
    }

    fn daemon(&self, plugin: &str, version: &str) -> Option<SharedDaemon> {
        self.registry.get(plugin, version)?.daemon()
    }

    fn start(
        &self,
        version: Version,
        daemon: SharedDaemon,
        serving: BTreeMap<BindingKey, Json>,
    ) -> Instance {
        let (control, inbox) = mpsc::unbounded_channel();
        let sockets = self
            .sockets
            .lock()
            .expect("supervisor sockets poisoned")
            .get(&version.0)
            .cloned();
        let actor = Actor {
            version: version.clone(),
            daemon,
            handles: Arc::clone(&self.handles),
            limits: self.limits,
            runtime: self.runtime.clone(),
            sockets,
            status: Arc::clone(&self.status),
            bindings: serving.clone(),
            leases: HashMap::new(),
            failures: VecDeque::new(),
            backoff: self.limits.backoff_min,
        };
        self.status
            .lock()
            .expect("supervisor status poisoned")
            .insert(version, Status::Running);
        let task = self.runtime.spawn(actor.run(inbox));
        Instance {
            control,
            serving,
            task,
        }
    }
}

/// How one `serve` call ended.
enum Ended {
    /// Asked to stop: the last binding went, or the server is shutting down.
    Stopped,
    /// Returned or panicked while bindings remained.
    Failed(String),
}

/// One instance: its bindings, its current `serve` call, its failures.
struct Actor {
    version: Version,
    daemon: SharedDaemon,
    handles: Arc<dyn Handles>,
    limits: Limits,
    runtime: tokio::runtime::Handle,
    sockets: Option<Arc<PluginSockets>>,
    status: Arc<Mutex<HashMap<Version, Status>>>,
    bindings: BTreeMap<BindingKey, Json>,
    /// The current handle of each binding, while an instance serves.
    leases: HashMap<BindingKey, Lease>,
    failures: VecDeque<Instant>,
    backoff: Duration,
}

impl Actor {
    async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Control>) {
        loop {
            if self.bindings.is_empty() {
                return;
            }
            let started = Instant::now();
            match self.serve(&mut inbox).await {
                Ended::Stopped => return,
                Ended::Failed(error) => {
                    tracing::warn!(
                        plugin = %self.version.0,
                        version = %self.version.1,
                        %error,
                        "plugin daemon failed"
                    );
                    if self.bindings.is_empty() {
                        return;
                    }
                    // A long healthy run earns a fresh backoff.
                    if started.elapsed() >= self.limits.backoff_max {
                        self.backoff = self.limits.backoff_min;
                    }
                    let now = Instant::now();
                    self.failures.push_back(now);
                    while self
                        .failures
                        .front()
                        .is_some_and(|at| now.duration_since(*at) > self.limits.failure_window)
                    {
                        self.failures.pop_front();
                    }
                    let failures = self.failures.len();
                    let keep_going = if failures >= self.limits.max_failures {
                        self.set(Status::Degraded { error, failures });
                        self.degraded(&mut inbox).await
                    } else {
                        self.set(Status::Restarting { error, failures });
                        let wait = self.backoff;
                        self.backoff = (self.backoff * 2).min(self.limits.backoff_max);
                        self.wait(&mut inbox, wait).await
                    };
                    if !keep_going {
                        return;
                    }
                }
            }
        }
    }

    fn set(&self, status: Status) {
        self.status
            .lock()
            .expect("supervisor status poisoned")
            .insert(self.version.clone(), status);
    }

    fn handle(&mut self, key: &BindingKey, config: &Json) -> SharedBinding {
        let lease = Lease::new();
        if let Some(old) = self.leases.insert(key.clone(), lease.clone()) {
            old.retire();
        }
        self.handles.handle(key.clone(), config.clone(), lease)
    }

    fn close(&mut self, key: &BindingKey) {
        if let Some(lease) = self.leases.remove(key) {
            lease.retire();
        }
        if let Some(sockets) = &self.sockets {
            sockets.close_binding(&key.id());
        }
    }

    fn close_all(&mut self) {
        let keys: Vec<BindingKey> = self.leases.keys().cloned().collect();
        for key in keys {
            self.close(&key);
        }
    }

    /// One `serve` call, from its `added` events to its end.
    async fn serve(&mut self, inbox: &mut mpsc::UnboundedReceiver<Control>) -> Ended {
        let (sender, events) = Events::channel(self.bindings.len() + 64);
        let daemon = Arc::clone(&self.daemon);
        let mut serving = self
            .runtime
            .spawn(async move { daemon.serve(events).await });
        self.set(Status::Running);

        let bindings: Vec<(BindingKey, Json)> = self
            .bindings
            .iter()
            .map(|(key, config)| (key.clone(), config.clone()))
            .collect();
        let mut early = None;
        for (key, config) in bindings {
            let handle = self.handle(&key, &config);
            if let Some(result) = deliver(&sender, Event::Added(handle), &mut serving).await {
                early = Some(Ended::Failed(failure(result)));
                break;
            }
        }

        let ended = match early {
            Some(ended) => ended,
            None => loop {
                tokio::select! {
                    result = &mut serving => break Ended::Failed(failure(result)),
                    control = inbox.recv() => match control {
                        None | Some(Control::Stop) => break Ended::Stopped,
                        Some(Control::Retry) => {}
                        Some(Control::Upsert(key, config)) => {
                            let event = if self.bindings.contains_key(&key) {
                                Event::Changed(self.handle(&key, &config))
                            } else {
                                Event::Added(self.handle(&key, &config))
                            };
                            self.bindings.insert(key, config);
                            if let Some(result) = deliver(&sender, event, &mut serving).await {
                                break Ended::Failed(failure(result));
                            }
                        }
                        Some(Control::Remove(key)) => {
                            if self.bindings.remove(&key).is_some() {
                                self.close(&key);
                                let event = Event::Removed(key.id());
                                if let Some(result) = deliver(&sender, event, &mut serving).await {
                                    break Ended::Failed(failure(result));
                                }
                            }
                            if self.bindings.is_empty() {
                                break Ended::Stopped;
                            }
                        }
                    },
                }
            },
        };

        if let Ended::Stopped = ended {
            // Closing the stream is the request to return.
            drop(sender);
            let abort = serving.abort_handle();
            match tokio::time::timeout(self.limits.grace, serving).await {
                Ok(Ok(Err(error))) => tracing::info!(
                    plugin = %self.version.0,
                    %error,
                    "plugin daemon stopped with an error"
                ),
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        plugin = %self.version.0,
                        grace = ?self.limits.grace,
                        "plugin daemon did not return within its grace; dropped"
                    );
                    abort.abort();
                }
            }
        } else {
            drop(sender);
        }
        self.close_all();
        ended
    }

    /// Waits out a backoff, keeping up with binding changes. `false` when
    /// asked to stop or left with no bindings.
    async fn wait(&mut self, inbox: &mut mpsc::UnboundedReceiver<Control>, wait: Duration) -> bool {
        let until = tokio::time::Instant::now() + wait;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(until) => return true,
                control = inbox.recv() => match control {
                    None | Some(Control::Stop) => return false,
                    Some(Control::Retry) => return true,
                    Some(change) => {
                        self.apply(change);
                        if self.bindings.is_empty() {
                            return false;
                        }
                    }
                },
            }
        }
    }

    /// Degraded: waits for a binding change or a retry, which clear the
    /// failure budget. `false` when asked to stop or left with no bindings.
    async fn degraded(&mut self, inbox: &mut mpsc::UnboundedReceiver<Control>) -> bool {
        let keep_going = match inbox.recv().await {
            None | Some(Control::Stop) => false,
            Some(Control::Retry) => true,
            Some(change) => {
                self.apply(change);
                !self.bindings.is_empty()
            }
        };
        self.failures.clear();
        self.backoff = self.limits.backoff_min;
        keep_going
    }

    /// A change that arrives while no `serve` call runs: the next start's
    /// `added` events carry it.
    fn apply(&mut self, change: Control) {
        match change {
            Control::Upsert(key, config) => {
                self.bindings.insert(key, config);
            }
            Control::Remove(key) => {
                self.bindings.remove(&key);
                self.close(&key);
            }
            Control::Retry | Control::Stop => {}
        }
    }
}

type Serving = JoinHandle<Result<(), String>>;
type Served = Result<Result<(), String>, tokio::task::JoinError>;

/// Sends an event, unless `serve` ends first: then its result.
async fn deliver(sender: &EventSender, event: Event, serving: &mut Serving) -> Option<Served> {
    tokio::select! {
        sent = sender.send(event) => match sent {
            Ok(()) => None,
            // The stream's receiver went with the `serve` call.
            Err(_) => Some(serving.await),
        },
        result = &mut *serving => Some(result),
    }
}

/// What a `serve` call that ended on its own counts as.
fn failure(result: Served) -> String {
    match result {
        Ok(Ok(())) => "serve returned while bindings remain".into(),
        Ok(Err(error)) => error,
        Err(error) if error.is_panic() => format!("serve panicked: {error}"),
        Err(error) => format!("serve was cancelled: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use plugin::daemon::{
        BindingHandle, Daemon, MemoryStore, SendError, SessionError, SessionRef, SharedStore,
    };
    use plugin::{Manifest, Message, Plugin};
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    /// A handle that only knows its id, config and whether it is current.
    struct FakeHandle {
        id: String,
        config: Json,
        lease: Lease,
    }

    #[async_trait]
    impl BindingHandle for FakeHandle {
        fn id(&self) -> &str {
            &self.id
        }
        fn config(&self) -> &Json {
            &self.config
        }
        async fn open(&self, _: &str, _: Option<&str>) -> Result<SessionRef, SessionError> {
            if self.lease.is_retired() {
                Err(SessionError::Gone)
            } else {
                Ok(SessionRef {
                    id: "s".into(),
                    created: true,
                })
            }
        }
        async fn send(&self, _: &str, _: plugin::daemon::Envelope, _: Option<&str>) -> Result<(), SendError> {
            Err(SendError::NotFound)
        }
        async fn archive(&self, _: &str) -> Result<(), SessionError> {
            Ok(())
        }
        fn storage(&self) -> SharedStore {
            MemoryStore::new(0)
        }
    }

    struct FakeHandles;

    impl Handles for FakeHandles {
        fn handle(&self, binding: BindingKey, config: Json, lease: Lease) -> SharedBinding {
            Arc::new(FakeHandle {
                id: binding.id(),
                config,
                lease,
            })
        }
    }

    /// Logs every event of every `serve` call, keeps the handles it was
    /// given, and fails its first `fail` starts.
    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<String>>,
        handles: Mutex<Vec<SharedBinding>>,
        starts: AtomicUsize,
        fail: AtomicUsize,
        panic: std::sync::atomic::AtomicBool,
        /// Ignore the stream closing, to test the grace period.
        stubborn: std::sync::atomic::AtomicBool,
    }

    impl Recorder {
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Daemon for Recorder {
        async fn serve(&self, mut events: Events) -> Result<(), String> {
            let start = self.starts.fetch_add(1, Ordering::SeqCst);
            self.log.lock().unwrap().push(format!("start {start}"));
            if self.fail.load(Ordering::SeqCst) > start {
                if self.panic.load(Ordering::SeqCst) {
                    panic!("boom {start}");
                }
                return Err(format!("failed {start}"));
            }
            while let Some(event) = events.next().await {
                let line = match &event {
                    Event::Added(handle) => format!("added {}", handle.config()),
                    Event::Changed(handle) => format!("changed {}", handle.config()),
                    Event::Removed(id) => format!("removed {}", id.split('/').nth(1).unwrap()),
                };
                if let Event::Added(handle) | Event::Changed(handle) = event {
                    self.handles.lock().unwrap().push(handle);
                }
                self.log.lock().unwrap().push(line);
            }
            self.log.lock().unwrap().push("closed".into());
            if self.stubborn.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(())
        }
    }

    struct WithDaemon {
        manifest: Manifest,
        daemon: Arc<Recorder>,
    }

    #[async_trait]
    impl Plugin for WithDaemon {
        fn manifest(&self) -> &Manifest {
            &self.manifest
        }
        fn daemon(&self) -> Option<SharedDaemon> {
            Some(self.daemon.clone())
        }
        async fn open(
            &self,
            _: &plugin::SessionCtx,
            _: plugin::Phase,
            _: &Json,
        ) -> Result<Vec<plugin::Content>, String> {
            Ok(Vec::new())
        }
        async fn handle_tool(
            &self,
            _: &plugin::SessionCtx,
            _: &Json,
            _: plugin::ToolCall,
        ) -> plugin::ToolResult {
            plugin::ToolResult::ok("")
        }
        fn validate(&self, config: Json) -> Result<Json, Vec<plugin::ConfigError>> {
            Ok(config)
        }
        fn migrate(&self, config: Json, _: u32) -> Result<Json, Vec<plugin::ConfigError>> {
            Ok(config)
        }
        fn on_changed(&self, _: &Json, _: &Json) -> Vec<Message> {
            Vec::new()
        }
        fn on_removed(&self, _: &Json) -> Vec<Message> {
            Vec::new()
        }
    }

    fn plugin(version: &str, daemon: Arc<Recorder>) -> Arc<WithDaemon> {
        Arc::new(WithDaemon {
            manifest: serde_json::from_value(json!({
                "id": "chat", "name": "Chat", "version": version, "description": "",
                "capabilities": { "daemon": {} },
                "config": { "version": 1, "schema": { "type": "object" }, "default": {} }
            }))
            .unwrap(),
            daemon,
        })
    }

    fn limits() -> Limits {
        Limits {
            grace: Duration::from_millis(100),
            backoff_min: Duration::from_millis(10),
            backoff_max: Duration::from_millis(40),
            max_failures: 3,
            failure_window: Duration::from_secs(60),
        }
    }

    fn supervisor(daemons: &[(&str, Arc<Recorder>)]) -> Supervisor {
        let mut registry = Registry::new();
        for (version, daemon) in daemons {
            registry
                .register(plugin(version, Arc::clone(daemon)), false)
                .unwrap();
        }
        Supervisor::new(Arc::new(registry), Arc::new(FakeHandles), limits())
    }

    fn desired(project: Uuid, version: &str, config: Json) -> Desired {
        Desired {
            binding: BindingKey::new(project, "chat"),
            version: version.into(),
            config,
        }
    }

    /// Polls until `check` passes, or fails the test after two seconds.
    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never: {what}");
    }

    #[tokio::test]
    async fn one_instance_gets_added_for_every_binding_then_changes_in_order() {
        let daemon = Arc::new(Recorder::default());
        let supervisor = supervisor(&[("1.0.0", daemon.clone())]);
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        supervisor
            .reconcile(vec![
                desired(a, "1.0.0", json!({ "n": "a" })),
                desired(b, "1.0.0", json!({ "n": "b" })),
            ])
            .await;
        eventually("both added", || daemon.log().len() == 3).await;
        assert_eq!(daemon.starts.load(Ordering::SeqCst), 1, "one per version");
        assert_eq!(supervisor.status("chat", "1.0.0"), Some(Status::Running));

        // A config change: a new handle, the old one is gone.
        supervisor
            .reconcile(vec![
                desired(a, "1.0.0", json!({ "n": "a2" })),
                desired(b, "1.0.0", json!({ "n": "b" })),
            ])
            .await;
        eventually("changed", || daemon.log().len() == 4).await;
        let first_a = daemon.handles.lock().unwrap()[0].clone();
        assert_eq!(first_a.open("k", None).await, Err(SessionError::Gone));
        let new_a = daemon.handles.lock().unwrap()[2].clone();
        assert!(new_a.open("k", None).await.is_ok());

        // Disabling one: removed. The same set again: nothing.
        supervisor
            .reconcile(vec![desired(a, "1.0.0", json!({ "n": "a2" }))])
            .await;
        supervisor
            .reconcile(vec![desired(a, "1.0.0", json!({ "n": "a2" }))])
            .await;
        eventually("removed", || daemon.log().len() == 5).await;
        let log = daemon.log();
        assert_eq!(log[0], "start 0");
        let mut added = log[1..3].to_vec();
        added.sort();
        assert_eq!(added, [r#"added {"n":"a"}"#, r#"added {"n":"b"}"#]);
        assert_eq!(log[3], r#"changed {"n":"a2"}"#);
        assert_eq!(log[4], "removed chat");

        // The last one goes: the stream closes and the instance stops.
        supervisor.reconcile(vec![]).await;
        eventually("closed", || daemon.log().last().unwrap() == "closed").await;
        assert_eq!(supervisor.status("chat", "1.0.0"), None);
        assert!(new_a.open("k", None).await.is_err());
    }

    #[tokio::test]
    async fn an_upgrade_removes_from_the_old_version_and_adds_to_the_new() {
        let old = Arc::new(Recorder::default());
        let new = Arc::new(Recorder::default());
        let supervisor = supervisor(&[("1.0.0", old.clone()), ("2.0.0", new.clone())]);
        let project = Uuid::now_v7();
        supervisor
            .reconcile(vec![desired(project, "1.0.0", json!({}))])
            .await;
        eventually("old added", || old.log().len() == 2).await;
        supervisor
            .reconcile(vec![desired(project, "2.0.0", json!({}))])
            .await;
        eventually("old closed", || old.log().last().unwrap() == "closed").await;
        eventually("new added", || new.log().len() == 2).await;
        assert_eq!(old.log()[2], "removed chat");
        assert_eq!(new.log()[1], "added {}");
    }

    #[tokio::test]
    async fn a_failing_daemon_restarts_with_every_binding_added_again() {
        let daemon = Arc::new(Recorder::default());
        daemon.fail.store(2, Ordering::SeqCst);
        let supervisor = supervisor(&[("1.0.0", daemon.clone())]);
        supervisor
            .reconcile(vec![desired(Uuid::now_v7(), "1.0.0", json!({}))])
            .await;
        eventually("third start serves", || daemon.log().len() == 4).await;
        assert_eq!(daemon.log(), ["start 0", "start 1", "start 2", "added {}"]);
        assert_eq!(supervisor.status("chat", "1.0.0"), Some(Status::Running));
    }

    #[tokio::test]
    async fn the_failure_budget_degrades_until_a_binding_changes_or_a_retry() {
        let daemon = Arc::new(Recorder::default());
        daemon.fail.store(6, Ordering::SeqCst);
        daemon.panic.store(true, Ordering::SeqCst);
        let supervisor = supervisor(&[("1.0.0", daemon.clone())]);
        let project = Uuid::now_v7();
        supervisor
            .reconcile(vec![desired(project, "1.0.0", json!({ "v": 1 }))])
            .await;
        eventually("degraded", || {
            matches!(
                supervisor.status("chat", "1.0.0"),
                Some(Status::Degraded { .. })
            )
        })
        .await;
        let Some(Status::Degraded { error, failures }) = supervisor.status("chat", "1.0.0") else {
            unreachable!()
        };
        assert_eq!(failures, 3);
        assert!(error.contains("panicked"), "{error}");
        // Not restarted on its own.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(daemon.starts.load(Ordering::SeqCst), 3);

        // A binding change clears the budget: three more tries.
        supervisor
            .reconcile(vec![desired(project, "1.0.0", json!({ "v": 2 }))])
            .await;
        eventually("degraded again", || {
            daemon.starts.load(Ordering::SeqCst) == 6
        })
        .await;
        eventually("degraded again", || {
            matches!(
                supervisor.status("chat", "1.0.0"),
                Some(Status::Degraded { .. })
            )
        })
        .await;

        // An operator's retry: the seventh start serves, with the new config.
        supervisor.retry("chat", "1.0.0").await;
        eventually("served", || {
            daemon.log().last().unwrap() == r#"added {"v":2}"#
        })
        .await;
        assert_eq!(supervisor.status("chat", "1.0.0"), Some(Status::Running));
    }

    #[tokio::test]
    async fn shutdown_closes_the_stream_and_drops_a_daemon_past_its_grace() {
        let polite = Arc::new(Recorder::default());
        let first = supervisor(&[("1.0.0", polite.clone())]);
        first
            .reconcile(vec![desired(Uuid::now_v7(), "1.0.0", json!({}))])
            .await;
        eventually("added", || polite.log().len() == 2).await;
        first.shutdown().await;
        assert_eq!(polite.log().last().unwrap(), "closed");

        let stubborn = Arc::new(Recorder::default());
        stubborn.stubborn.store(true, Ordering::SeqCst);
        let second = supervisor(&[("1.0.0", stubborn.clone())]);
        second
            .reconcile(vec![desired(Uuid::now_v7(), "1.0.0", json!({}))])
            .await;
        eventually("added", || stubborn.log().len() == 2).await;
        let began = Instant::now();
        second.shutdown().await;
        let took = began.elapsed();
        assert!(took >= Duration::from_millis(100), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
        let handle = stubborn.handles.lock().unwrap()[0].clone();
        assert_eq!(handle.open("k", None).await, Err(SessionError::Gone));
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn the_desired_set_is_every_enabled_binding_of_a_daemon_version() {
        use diesel_async::RunQueryDsl;
        let fixture = crate::plugins::daemon::fixture::Fixture::new(0).await;
        let mut registry = Registry::new();
        registry
            .register(plugin("1.0.0", Arc::new(Recorder::default())), false)
            .unwrap();
        let other = fixture.project("off").await;
        let mut conn = fixture.pool.get().await.unwrap();
        for (project, plugin_type, version, enabled) in [
            (fixture.project, "chat", "1.0.0", true),
            (other, "chat", "1.0.0", false),
            // A version this server does not have, and a plugin with no daemon.
            (fixture.project, "chat-old", "0.1.0", true),
        ] {
            diesel::sql_query(
                "INSERT INTO project_binding \
                 (project_id, plugin_type, version, config_version, config, enabled) \
                 VALUES ($1, $2, $3, 1, '{\"k\": 1}', $4)",
            )
            .bind::<diesel::sql_types::Uuid, _>(project)
            .bind::<diesel::sql_types::Text, _>(plugin_type)
            .bind::<diesel::sql_types::Text, _>(version)
            .bind::<diesel::sql_types::Bool, _>(enabled)
            .execute(&mut conn)
            .await
            .unwrap();
        }
        drop(conn);
        let desired = crate::plugins::daemon::desired(&fixture.pool, &registry)
            .await
            .unwrap();
        let ours: Vec<&Desired> = desired
            .iter()
            .filter(|row| row.binding.project == fixture.project || row.binding.project == other)
            .collect();
        assert_eq!(
            ours,
            [&Desired {
                binding: BindingKey::new(fixture.project, "chat"),
                version: "1.0.0".into(),
                config: json!({ "k": 1 }),
            }]
        );
        fixture.cleanup().await;
    }
}
