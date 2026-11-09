//! The seam between the Environments plugin and whatever runs scratch.
//!
//! Scratch is one environment per session: real processes in the session's
//! container in tenantd, started when a call first needs it. The plugin only
//! asks for a place to run one call and passes along what kind of call it
//! is; everything about starting the container, idle teardown and archiving
//! lives behind [`ScratchRuntime`].
//!
//! [`Sandboxed`] is the runtime: `crates/scratch`'s manager, configured from
//! the operator's settings and nothing else. No variable of this process's
//! own environment reaches a session: the container starts from the image's
//! fixed environment.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use environment::{
    Blob, Blobs, Chunk, Cursor, Denial, Edit, Exec, Exit, Fault, Listing, Manifest, ProcId,
    Process, RootedPath, Signal, Span, Stat, Target, Window,
};
use scratch::{ScratchConfig, ScratchManager, Session, TenantdConfig};

use super::places::{Bound, RecentBlobs, Unavailable};
use crate::config::ScratchSettings;

/// What a call needs of its session, so the runtime can start the container
/// before any of it runs.
pub use scratch::Call;

/// Whose scratch a call is for. Scratch belongs to the session; the owner is
/// carried for the per-user quota and tenant, the project for the volumes
/// and sandbox its sessions share.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScratchKey {
    pub session: String,
    pub owner: uuid::Uuid,
    pub project: uuid::Uuid,
}

/// Where one scratch call runs.
pub struct Routed {
    pub bound: Arc<Bound>,
    /// This is the first call to reach the session's scratch since this
    /// process opened it — a new session, or one restored after a restart —
    /// so its mounts have yet to be re-established from their declarations.
    /// A call that fails before it runs does not count, so the next one
    /// still restores them.
    pub opened: bool,
    /// Held for as long as the call runs: deleting the session waits for no
    /// call, but a call still holding this when its session is deleted
    /// deletes whatever it left behind when it ends.
    pub guard: CallGuard,
}

/// Calls in flight on one session's scratch, and whether that session has
/// been deleted under them.
struct InFlight {
    session: String,
    calls: AtomicUsize,
    deleted: AtomicBool,
    manager: Arc<ScratchManager>,
}

/// One call in flight; see [`Routed::guard`].
pub struct CallGuard(Option<Arc<InFlight>>);

impl CallGuard {
    /// A guard no deletion has to clean up after, for a runtime that keeps
    /// nothing on disk.
    pub fn none() -> Self {
        CallGuard(None)
    }

    fn new(calls: &Arc<InFlight>) -> Self {
        calls.calls.fetch_add(1, Ordering::SeqCst);
        CallGuard(Some(Arc::clone(calls)))
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        let Some(calls) = self.0.take() else {
            return;
        };
        // The last call of a deleted session: whatever it wrote after the
        // deletion is deleted now, so no scratch outlives its session.
        if calls.calls.fetch_sub(1, Ordering::SeqCst) == 1
            && calls.deleted.load(Ordering::SeqCst)
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                calls.manager.delete_session(&calls.session).await;
            });
        }
    }
}

/// One mount as the runtime serves it.
pub struct LiveMount {
    pub plugin: String,
    pub name: String,
    pub read_only: bool,
    pub backend: Arc<dyn scratch::MountBackend>,
}

/// The scratch runtime the plugin drives.
///
/// Every method is per session. Two sessions never share a place, a file or a
/// process; the runtime keys everything by [`ScratchKey::session`].
#[async_trait]
pub trait ScratchRuntime: Send + Sync + 'static {
    /// Why scratch cannot run on this server at all, for the head and for
    /// every call; `None` when it can.
    fn unavailable(&self) -> Option<String>;

    /// The place this call runs, opening or restoring the session's scratch
    /// when it is not open, and starting its container when the call needs
    /// one. An error has run nothing.
    async fn route(&self, key: &ScratchKey, call: Call) -> Result<Routed, Unavailable>;

    /// The sessions whose scratch this process has open.
    async fn open_sessions(&self) -> Vec<String>;

    /// The sessions with a scratch kept in tenantd, open or not: what a sweep
    /// for scratches whose session is gone looks through.
    async fn stored_sessions(&self) -> Vec<String>;

    /// Marks processes `process start` started in the session's scratch:
    /// while one runs, the session is not idle. Other processes — what an
    /// `exec` left running — are stopped with an idle sandbox.
    async fn manage(&self, key: &ScratchKey, ids: &[ProcId]);

    /// Replaces what the session's scratch serves under `/mnt`. Nothing
    /// happens for a session whose scratch is not open: opening it
    /// re-establishes its mounts.
    async fn set_mounts(&self, session: &str, mounts: Vec<LiveMount>);

    /// Deletes what an earlier process of this server left running. Called
    /// once at startup, before any session is opened.
    async fn recover(&self);

    /// Lets what has been idle long enough go idle — its container stopped,
    /// its ephemeral files dropped, the project's volumes kept — and lets go
    /// of what this process holds for sessions no one has called since.
    async fn reap_idle(&self);

    /// Deletes the session's scratch with everything in it. Called when the
    /// session is deleted; no later call reaches it, and a call still
    /// running cleans up after itself when it ends.
    async fn delete(&self, session: &str);
}

/// A session's scratch as this process holds it.
struct Opened {
    session: Arc<Session>,
    bound: Arc<Bound>,
    /// (plugin, name) of every mount the session serves now.
    mounted: Vec<(String, String)>,
    /// A call has been routed here successfully, so the mounts were handed
    /// over to be restored.
    restored: bool,
    calls: Arc<InFlight>,
    /// When a call was last routed here.
    last: Instant,
}

/// Scratch over `crates/scratch`.
pub struct Sandboxed {
    /// The manager, or why there is none — told to the agent as is.
    manager: Result<Arc<ScratchManager>, String>,
    opened: tokio::sync::Mutex<HashMap<String, Opened>>,
    /// Sessions deleted while this process ran. A call that was already on
    /// its way when its session went is refused rather than opening a fresh
    /// scratch no session owns.
    deleted: std::sync::Mutex<HashSet<String>>,
}

/// What the agent is told when the operator configured no scratch, or one
/// that could not open. The detail is the operator's, and goes to the log.
const NOT_AVAILABLE: &str = "scratch is not available on this Faber server";

impl Sandboxed {
    /// Scratch from the operator's settings. Without tenantd and an image
    /// there is none, and every call says so.
    pub fn new(settings: &ScratchSettings) -> Self {
        if let Some(invalid) = &settings.invalid {
            tracing::error!("{invalid}; scratch is off");
            return Sandboxed::off();
        }
        let (endpoint, image) = match (&settings.tenantd, &settings.image) {
            (Some(endpoint), Some(image)) => (endpoint, image),
            (None, None) => return Sandboxed::off(),
            _ => {
                tracing::error!(
                    "scratch needs both tenantd (FABER_TENANTD_SOCKET or FABER_TENANTD_ENDPOINT) and FABER_SCRATCH_IMAGE; it is off"
                );
                return Sandboxed::off();
            }
        };
        tracing::info!(%endpoint, "scratch runs in tenantd");
        let mut tenantd = TenantdConfig::new(endpoint.clone(), image);
        if let Some(profile) = &settings.profile {
            tenantd.profile = profile.clone();
        }
        if let Some(bytes) = settings.quota_bytes {
            tenantd.storage_bytes = bytes;
        }
        Sandboxed::with_config(ScratchConfig::new(tenantd))
    }

    /// Scratch over an exact configuration.
    pub fn with_config(config: ScratchConfig) -> Self {
        Sandboxed {
            manager: Ok(Arc::new(ScratchManager::new(config))),
            opened: tokio::sync::Mutex::new(HashMap::new()),
            deleted: std::sync::Mutex::new(HashSet::new()),
        }
    }

    /// No scratch on this server.
    pub fn off() -> Self {
        Sandboxed {
            manager: Err(NOT_AVAILABLE.to_owned()),
            opened: tokio::sync::Mutex::new(HashMap::new()),
            deleted: std::sync::Mutex::new(HashSet::new()),
        }
    }

    /// Lets go of sessions no call has reached for `idle`, with no call in
    /// flight and no process still running. Their files stay in tenantd; the
    /// next call opens them again and re-establishes their mounts. A session
    /// with a running process is kept, so its process ids stay valid.
    async fn forget_idle(&self, idle: std::time::Duration) {
        let quiet = |opened: &Opened| {
            opened.last.elapsed() >= idle && opened.calls.calls.load(Ordering::SeqCst) == 0
        };
        let candidates: Vec<(String, Arc<Session>)> = self
            .opened
            .lock()
            .await
            .iter()
            .filter(|(_, opened)| quiet(opened))
            .map(|(id, opened)| (id.clone(), Arc::clone(&opened.session)))
            .collect();
        for (id, session) in candidates {
            let running = match session.processes().await {
                Ok(processes) => processes.iter().any(|process| process.outcome.is_none()),
                // Kept open: closing could kill work nobody can see. Logged
                // because a session in this state never idles out.
                Err(error) => {
                    tracing::warn!(session = %id, %error, "could not list a session's processes; not closing it");
                    true
                }
            };
            if running {
                continue;
            }
            let mut sessions = self.opened.lock().await;
            if sessions.get(&id).is_some_and(quiet) {
                sessions.remove(&id);
            }
        }
    }

    fn manager(&self) -> Result<&Arc<ScratchManager>, Unavailable> {
        self.manager
            .as_ref()
            .map_err(|why| Unavailable::Internal(why.clone()))
    }
}

/// The reason a refusal from scratch gives, without the layer's framing.
fn reason(fault: &Fault) -> String {
    match fault {
        Fault::Denied(Denial::Malformed { reason, .. }) => reason.to_string(),
        Fault::Unreachable(reason) => format!("scratch could not start its container: {reason}"),
        other => other.to_string(),
    }
}

#[async_trait]
impl ScratchRuntime for Sandboxed {
    fn unavailable(&self) -> Option<String> {
        self.manager.as_ref().err().cloned()
    }

    async fn route(&self, key: &ScratchKey, call: Call) -> Result<Routed, Unavailable> {
        let manager = self.manager()?;
        let (session, bound, guard) = {
            let mut sessions = self.opened.lock().await;
            // Checked under the same lock `delete` takes, so a call cannot
            // slip in between the deletion and its tombstone.
            if self
                .deleted
                .lock()
                .expect("deleted sessions poisoned")
                .contains(&key.session)
            {
                return Err(Unavailable::Internal(
                    "this session was deleted, and its scratch with it".to_owned(),
                ));
            }
            let found = match sessions.entry(key.session.clone()) {
                std::collections::hash_map::Entry::Occupied(found) => found.into_mut(),
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    let blobs = Arc::new(RecentBlobs::default());
                    let session = manager
                        .session(
                            &key.session,
                            &key.owner.to_string(),
                            &key.project.to_string(),
                            Arc::clone(&blobs) as Arc<dyn Blobs>,
                        )
                        .await
                        .map_err(|error| Unavailable::Internal(error.to_string()))?;
                    let bound = Arc::new(Bound::new(
                        "scratch",
                        Arc::new(ScratchTarget(Arc::clone(&session))) as Arc<dyn Target>,
                        blobs,
                    ));
                    vacant.insert(Opened {
                        session,
                        bound,
                        mounted: Vec::new(),
                        restored: false,
                        calls: Arc::new(InFlight {
                            session: key.session.clone(),
                            calls: AtomicUsize::new(0),
                            deleted: AtomicBool::new(false),
                            manager: Arc::clone(manager),
                        }),
                        last: Instant::now(),
                    })
                }
            };
            found.last = Instant::now();
            (
                Arc::clone(&found.session),
                Arc::clone(&found.bound),
                CallGuard::new(&found.calls),
            )
        };
        session
            .prepare(call)
            .await
            .map_err(|fault| Unavailable::Internal(reason(&fault)))?;
        // Only now, with the call about to run, are the mounts handed over
        // to be restored: a first call that failed before running leaves
        // that to the next one.
        let opened = match self.opened.lock().await.get_mut(&key.session) {
            Some(found) if !found.restored => {
                found.restored = true;
                true
            }
            _ => false,
        };
        Ok(Routed {
            bound,
            opened,
            guard,
        })
    }

    async fn open_sessions(&self) -> Vec<String> {
        self.opened.lock().await.keys().cloned().collect()
    }

    async fn stored_sessions(&self) -> Vec<String> {
        let Ok(manager) = &self.manager else {
            return Vec::new();
        };
        manager.stored_sessions().await
    }

    async fn manage(&self, key: &ScratchKey, ids: &[ProcId]) {
        if let Some(opened) = self.opened.lock().await.get(&key.session) {
            for id in ids {
                opened.session.manage(*id);
            }
        }
    }

    async fn set_mounts(&self, session: &str, mounts: Vec<LiveMount>) {
        let mut sessions = self.opened.lock().await;
        let Some(opened) = sessions.get_mut(session) else {
            return;
        };
        for (plugin, name) in &opened.mounted {
            if !mounts
                .iter()
                .any(|mount| &mount.plugin == plugin && &mount.name == name)
            {
                let _ = opened.session.unmount(plugin, name);
            }
        }
        let mut mounted = Vec::new();
        for mount in mounts {
            match opened
                .session
                .mount(&mount.plugin, &mount.name, mount.read_only, mount.backend)
            {
                Ok(_) => mounted.push((mount.plugin, mount.name)),
                Err(error) => tracing::warn!(
                    %session, plugin = %mount.plugin, name = %mount.name, ?error,
                    "a declared mount could not be served"
                ),
            }
        }
        opened.mounted = mounted;
    }

    async fn recover(&self) {
        if let Ok(manager) = &self.manager {
            manager.recover().await;
        }
    }

    async fn reap_idle(&self) {
        let Ok(manager) = &self.manager else {
            return;
        };
        manager.teardown_idle().await;
        self.forget_idle(manager.config().idle_timeout).await;
    }

    async fn delete(&self, session: &str) {
        let removed = {
            let mut sessions = self.opened.lock().await;
            self.deleted
                .lock()
                .expect("deleted sessions poisoned")
                .insert(session.to_owned());
            sessions.remove(session)
        };
        if let Some(removed) = removed {
            removed.calls.deleted.store(true, Ordering::SeqCst);
        }
        if let Ok(manager) = &self.manager {
            manager.delete_session(session).await;
        }
    }
}

/// A session's scratch as the tools see it. `exec` starts its command as a
/// process so it can outlive the call's wait; here that start runs in the
/// without marking it as managed, which only `process start` does.
struct ScratchTarget(Arc<Session>);

#[async_trait]
impl Target for ScratchTarget {
    fn manifest(&self) -> &Manifest {
        self.0.manifest()
    }

    async fn exec(&self, req: Exec) -> Result<Exit, Fault> {
        self.0.exec(req).await
    }

    async fn start(&self, req: Exec) -> Result<ProcId, Fault> {
        self.0.begin(req).await
    }

    async fn output(&self, id: ProcId, from: Cursor) -> Result<Chunk, Fault> {
        self.0.output(id, from).await
    }

    async fn stdin(&self, id: ProcId, body: &Blob) -> Result<(), Fault> {
        self.0.stdin(id, body).await
    }

    async fn close_stdin(&self, id: ProcId) -> Result<(), Fault> {
        self.0.close_stdin(id).await
    }

    async fn processes(&self) -> Result<Vec<Process>, Fault> {
        self.0.processes().await
    }

    async fn signal(&self, id: ProcId, signal: Signal) -> Result<(), Fault> {
        self.0.signal(id, signal).await
    }

    async fn read(&self, path: &RootedPath, window: Option<Window>) -> Result<Span, Fault> {
        self.0.read(path, window).await
    }

    async fn write(&self, path: &RootedPath, body: &Blob) -> Result<Stat, Fault> {
        self.0.write(path, body).await
    }

    async fn edit(&self, edit: &Edit) -> Result<Vec<Stat>, Fault> {
        self.0.edit(edit).await
    }

    async fn list(&self, path: &RootedPath, glob: Option<&str>) -> Result<Listing, Fault> {
        self.0.list(path, glob).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::tools;
    use super::*;

    fn key() -> ScratchKey {
        ScratchKey {
            session: uuid::Uuid::now_v7().to_string(),
            owner: uuid::Uuid::now_v7(),
            project: uuid::Uuid::now_v7(),
        }
    }

    #[tokio::test]
    async fn a_server_without_tenantd_or_an_image_has_no_scratch() {
        for settings in [
            ScratchSettings::default(),
            ScratchSettings {
                image: Some("registry.local/faber/scratch".to_owned()),
                ..ScratchSettings::default()
            },
            ScratchSettings {
                tenantd: Some(scratch::Endpoint::Socket("/run/tenantd/api.sock".into())),
                ..ScratchSettings::default()
            },
        ] {
            let scratch = Sandboxed::new(&settings);
            assert_eq!(scratch.unavailable().as_deref(), Some(NOT_AVAILABLE));
            assert!(matches!(
                scratch.route(&key(), Call::Process).await,
                Err(Unavailable::Internal(_))
            ));
        }
    }

    #[tokio::test]
    async fn a_call_tenantd_cannot_answer_fails_having_run_nothing() {
        let scratch = Sandboxed::with_config(ScratchConfig::new(TenantdConfig::new(
            scratch::Endpoint::Socket("/nonexistent/tenantd.sock".into()),
            "registry.local/faber/scratch",
        )));
        assert_eq!(scratch.unavailable(), None);
        assert!(matches!(
            scratch.route(&key(), Call::Container).await,
            Err(Unavailable::Internal(_))
        ));
    }

    /// A manager over the tenantd `FABER_TEST_TENANTD` and `FABER_TEST_IMAGE`
    /// name, as for `crates/scratch`'s compute tests.
    fn real() -> Sandboxed {
        let (Some(endpoint), Ok(image)) = (
            scratch::compute::test_endpoint(),
            std::env::var("FABER_TEST_IMAGE"),
        ) else {
            panic!("set FABER_TEST_TENANTD and FABER_TEST_IMAGE");
        };
        let mut config = ScratchConfig::new(TenantdConfig::new(endpoint, image));
        config.shell = "/bin/sh".to_owned();
        Sandboxed::with_config(config)
    }

    /// Runs `exec` as the plugin does: routed, then dispatched.
    async fn exec(scratch: &Sandboxed, key: &ScratchKey, command: &str) -> String {
        let input = json!({
            "execute_in": "scratch",
            "command": command,
            "cwd": "/root",
        });
        let routed = scratch
            .route(key, tools::call_kind("exec", &input))
            .await
            .unwrap();
        let place = tools::Place {
            bound: &routed.bound,
            name: "scratch",
        };
        let mut effects = tools::Effects::default();
        let result = tools::dispatch(&place, "exec", &input, &mut effects).await;
        assert!(!result.is_error(), "{}", plugin::joined(&result.content));
        plugin::joined(&result.content)
    }

    #[tokio::test]
    #[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
    async fn a_command_runs_in_the_container_with_no_server_variable() {
        let scratch = real();
        let key = key();
        // Whatever this process has, the session sees the image's own.
        let out = exec(&scratch, &key, "echo \"home=$HOME user=$USER path=$PATH\"").await;
        assert!(out.contains("home=/root "), "{out}");
        assert!(
            out.contains("path=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"),
            "{out}"
        );
        for (name, value) in std::env::vars() {
            if name == "USER" && !value.is_empty() && value != "root" {
                assert!(!out.contains(&format!("user={value}")), "{out}");
            }
        }
        scratch.delete(&key.session).await;
    }

    #[tokio::test]
    #[ignore = "needs tenantd: set FABER_TEST_TENANTD and FABER_TEST_IMAGE"]
    async fn deleting_a_session_removes_its_scratch() {
        let scratch = real();
        let key = key();
        exec(&scratch, &key, "echo kept > \"$TMPDIR/notes\"").await;
        assert!(scratch.stored_sessions().await.contains(&key.session));
        scratch.delete(&key.session).await;
        assert!(!scratch.stored_sessions().await.contains(&key.session));
    }
}
