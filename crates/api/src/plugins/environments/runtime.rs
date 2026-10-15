//! The seam between the Environments plugin and whatever runs scratch.
//!
//! Scratch is one environment per session with two modes: virtual (a shell
//! interpreted over scratch's own layer) and compute (real processes in a
//! sandbox). Which mode a call runs in is the runtime's decision, made before
//! anything runs, and every result says which it was. The plugin only asks
//! for a place to run one call and passes along what kind of call it is;
//! everything about modes, promotion, idle teardown and archiving lives
//! behind [`ScratchRuntime`].
//!
//! [`Sandboxed`] is the runtime: `crates/scratch`'s manager, configured from
//! the operator's settings and nothing else. No variable of this process's
//! own environment reaches a session: the virtual shell and the sandbox both
//! start from the image's fixed environment.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use environment::{
    Blob, Blobs, Chunk, Cursor, Denial, Edit, Exec, Exit, Fault, Listing, Manifest, ProcId,
    Process, RootedPath, Signal, Span, Stat, Target, Window,
};
use scratch::{ScratchConfig, ScratchManager, Session};

use super::places::{Bound, RecentBlobs, Unavailable};
use crate::config::ScratchSettings;

/// What a call is about to do, so the runtime can pick (or switch to) the
/// mode that will run it before any of it runs.
pub use scratch::Call;

/// Which of scratch's two modes ran a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The virtual shell over scratch's layer.
    Virtual,
    /// Real processes in the sandbox.
    Compute,
}

impl Mode {
    /// The first line of every scratch result.
    pub fn line(self) -> &'static str {
        match self {
            Mode::Virtual => "mode: virtual",
            Mode::Compute => "mode: compute",
        }
    }
}

impl From<scratch::Mode> for Mode {
    fn from(mode: scratch::Mode) -> Self {
        match mode {
            scratch::Mode::Virtual => Mode::Virtual,
            scratch::Mode::Compute => Mode::Compute,
        }
    }
}

/// Whose scratch a call is for. Scratch belongs to the session; the owner is
/// carried for the per-user quota.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScratchKey {
    pub session: String,
    pub owner: uuid::Uuid,
}

/// Where one scratch call runs.
pub struct Routed {
    pub bound: Arc<Bound>,
    /// The mode that runs the call, settled before any of it runs. A
    /// virtual run that fails is a failed virtual run; nothing is run again
    /// in compute after the fact.
    pub mode: Mode,
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
        // deletion (a virtual write recreates the directories it needs) is
        // deleted now, so no scratch outlives its session.
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
    /// when it is not open, and promoting it when the call needs compute.
    /// An error has run nothing.
    async fn route(&self, key: &ScratchKey, call: Call<'_>) -> Result<Routed, Unavailable>;

    /// The session's mode now; read after a call, the mode that ran it.
    /// `None` when the session's scratch is not open.
    async fn mode(&self, key: &ScratchKey) -> Option<Mode>;

    /// The sessions whose scratch this process has open.
    async fn open_sessions(&self) -> Vec<String>;

    /// The sessions with a scratch kept on disk, open or not: what a sweep
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

    /// Archives and stops what an earlier process of this server left
    /// running. Called once at startup, before any session is opened.
    async fn recover(&self);

    /// Stops what has been idle long enough, keeping its files, and lets go
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
    /// Scratch from the operator's settings. Without a root and a base image
    /// there is none, and every call says so.
    pub fn new(settings: &ScratchSettings) -> Self {
        if let Some(invalid) = &settings.invalid {
            tracing::error!("{invalid}; scratch is off");
            return Sandboxed::off();
        }
        let (Some(root), Some(rootfs)) = (&settings.root, &settings.rootfs) else {
            if settings.root.is_some() || settings.rootfs.is_some() {
                tracing::error!(
                    "scratch needs both FABER_SCRATCH_ROOT and FABER_SCRATCH_ROOTFS; it is off"
                );
            }
            return Sandboxed::off();
        };
        let mut config = ScratchConfig::new(root, rootfs);
        config.toolstore = settings.toolstore.clone();
        config.runsc = settings.runsc.clone();
        config.helper = settings.helper.clone();
        config.quota = settings.quota_bytes;
        // runsc needs `--rootless` exactly when this process cannot make
        // the namespaces and mounts a sandbox needs as root.
        // Safety: geteuid has no preconditions.
        config.rootless = unsafe { libc::geteuid() } != 0;
        if config.runsc.is_none() {
            tracing::warn!("FABER_RUNSC is unset: scratch runs virtual only, with no compute");
        } else if config.helper.is_none() {
            tracing::warn!(
                "FABER_SCRATCH_HELPER is unset: scratch compute runs without /mnt, and its files are read through the image's own tools"
            );
        }
        Sandboxed::with_config(config)
    }

    /// Scratch over an exact configuration.
    pub fn with_config(config: ScratchConfig) -> Self {
        let manager = ScratchManager::new(config).map(Arc::new).map_err(|error| {
            tracing::error!(%error, "scratch could not open; it is off");
            NOT_AVAILABLE.to_owned()
        });
        Sandboxed {
            manager,
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
    /// flight and no process still running. Their files stay on disk; the
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
                Err(_) => true,
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
            .map_err(|why| Unavailable::Unreachable(why.clone()))
    }
}

/// The reason a refusal from scratch gives, without the layer's framing.
fn reason(fault: &Fault) -> String {
    match fault {
        Fault::Denied(Denial::Malformed { reason, .. }) => reason.to_string(),
        Fault::Unreachable(reason) => format!("scratch could not start its sandbox: {reason}"),
        other => other.to_string(),
    }
}

#[async_trait]
impl ScratchRuntime for Sandboxed {
    fn unavailable(&self) -> Option<String> {
        self.manager.as_ref().err().cloned()
    }

    async fn route(&self, key: &ScratchKey, call: Call<'_>) -> Result<Routed, Unavailable> {
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
                return Err(Unavailable::Unreachable(
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
                            Arc::clone(&blobs) as Arc<dyn Blobs>,
                        )
                        .await
                        .map_err(|error| Unavailable::Unreachable(error.to_string()))?;
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
        let mode = session
            .prepare(call)
            .await
            .map_err(|fault| Unavailable::Unreachable(reason(&fault)))?;
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
            mode: mode.into(),
            opened,
            guard,
        })
    }

    async fn mode(&self, key: &ScratchKey) -> Option<Mode> {
        let session = Arc::clone(&self.opened.lock().await.get(&key.session)?.session);
        Some(session.mode().await.into())
    }

    async fn open_sessions(&self) -> Vec<String> {
        self.opened.lock().await.keys().cloned().collect()
    }

    async fn stored_sessions(&self) -> Vec<String> {
        let Ok(manager) = &self.manager else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(manager.config().root.join("sessions")) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect()
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
/// session's current mode instead of always promoting, which only `process
/// start` does (its call was promoted when it was routed).
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
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::super::tools;
    use super::*;

    fn key() -> ScratchKey {
        ScratchKey {
            session: uuid::Uuid::now_v7().to_string(),
            owner: uuid::Uuid::now_v7(),
        }
    }

    /// A directory removed when dropped.
    struct Temp(PathBuf);

    impl Temp {
        fn new(name: &str) -> Temp {
            let dir = std::env::temp_dir().join(format!(
                "faber-api-scratch-{name}-{}-{}",
                std::process::id(),
                rand::random::<u32>()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A base image with the directories both modes expect.
    fn base_image(dir: &Path) {
        for sub in [
            "etc", "usr/bin", "root", "tmp", "mnt", "proc", "dev", "sys", "opt",
        ] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        symlink("usr/bin", dir.join("bin")).unwrap();
        std::fs::write(dir.join("etc/os-release"), "ID=test\n").unwrap();
        // gVisor mounts a tmpfs over an empty /tmp.
        std::fs::write(dir.join("tmp/.keep"), "").unwrap();
    }

    fn config(dir: &Temp) -> ScratchConfig {
        let rootfs = dir.0.join("rootfs");
        base_image(&rootfs);
        ScratchConfig::new(dir.0.join("scratch"), rootfs)
    }

    /// Runs `exec` as the plugin does: routed, dispatched, and labelled with
    /// the mode settled before it ran.
    async fn exec(scratch: &Sandboxed, key: &ScratchKey, command: &str, compute: bool) -> String {
        let result = exec_result(scratch, key, command, compute).await;
        assert!(!result.is_error(), "{}", plugin::joined(&result.content));
        plugin::joined(&result.content)
    }

    async fn exec_result(
        scratch: &Sandboxed,
        key: &ScratchKey,
        command: &str,
        compute: bool,
    ) -> plugin::ToolResult {
        let input = json!({
            "execute_in": "scratch",
            "command": command,
            "cwd": "/root",
            "compute": compute,
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
        tools::with_mode(result, Some(routed.mode))
    }

    #[tokio::test]
    async fn a_failing_virtual_command_stays_virtual_and_promotes_nothing() {
        let dir = Temp::new("failing");
        let scratch = Sandboxed::with_config(config(&dir));
        let key = key();

        let result = exec_result(&scratch, &key, "cat /nope", false).await;
        let out = plugin::joined(&result.content);
        assert!(out.starts_with("mode: virtual"), "{out}");
        assert!(
            out.contains("cat: /nope: No such file or directory"),
            "{out}"
        );
        assert!(!out.contains("io error"), "{out}");
        assert_eq!(scratch.mode(&key).await, Some(Mode::Virtual));

        let out = exec(&scratch, &key, "echo next", false).await;
        assert!(out.starts_with("mode: virtual"), "{out}");
        scratch.delete(&key.session).await;
    }

    #[tokio::test]
    async fn a_server_without_a_root_or_base_image_has_no_scratch() {
        let scratch = Sandboxed::new(&ScratchSettings {
            root: Some(std::env::temp_dir()),
            ..ScratchSettings::default()
        });
        assert_eq!(scratch.unavailable().as_deref(), Some(NOT_AVAILABLE));
        assert!(matches!(
            scratch.route(&key(), Call::Read).await,
            Err(Unavailable::Unreachable(_))
        ));
    }

    #[tokio::test]
    async fn a_simple_command_runs_virtual_and_says_so() {
        let dir = Temp::new("virtual");
        let scratch = Sandboxed::with_config(config(&dir));
        let key = key();
        let out = exec(&scratch, &key, "echo hello from scratch", false).await;
        assert!(out.starts_with("mode: virtual"), "{out}");
        assert!(out.contains("hello from scratch"), "{out}");
    }

    #[tokio::test]
    async fn no_server_variable_reaches_the_virtual_shell() {
        let dir = Temp::new("ambient");
        let scratch = Sandboxed::with_config(config(&dir));
        let key = key();
        // Whatever this process has, the session sees the image's own.
        let out = exec(
            &scratch,
            &key,
            "echo \"home=$HOME user=$USER path=$PATH\"",
            false,
        )
        .await;
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
    }

    #[tokio::test]
    async fn compute_without_a_sandbox_runtime_fails_having_run_nothing() {
        let dir = Temp::new("no-runsc");
        let scratch = Sandboxed::with_config(config(&dir));
        let key = key();
        let refused = scratch
            .route(
                &key,
                Call::Exec {
                    command: "touch /root/ran",
                    compute: true,
                },
            )
            .await;
        match refused {
            Err(Unavailable::Unreachable(reason)) => {
                assert!(reason.contains("no sandbox runtime"), "{reason}")
            }
            _ => panic!("compute was not refused"),
        }
        assert_eq!(scratch.mode(&key).await, Some(Mode::Virtual));
        let out = exec(&scratch, &key, "ls /root", false).await;
        assert!(!out.contains("ran"), "{out}");
    }

    #[tokio::test]
    async fn deleting_a_session_removes_its_scratch() {
        let dir = Temp::new("delete");
        let config = config(&dir);
        let root = config.root.clone();
        let scratch = Sandboxed::with_config(config);
        let key = key();
        exec(&scratch, &key, "echo kept > /root/notes", false).await;
        assert!(root.join("sessions").join(&key.session).exists());
        scratch.delete(&key.session).await;
        assert!(!root.join("sessions").join(&key.session).exists());
        assert_eq!(scratch.mode(&key).await, None);
    }

    fn env_path(name: &str) -> Option<PathBuf> {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.exists())
    }

    /// Promotion against a real runsc, with `FABER_TEST_RUNSC`,
    /// `FABER_TEST_HELPER` and `FABER_TEST_BUSYBOX` naming the pieces, as for
    /// `crates/scratch`'s compute tests.
    #[tokio::test]
    #[ignore = "needs runsc: set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX"]
    async fn compute_true_promotes_and_every_result_says_compute_after() {
        let (Some(runsc), Some(helper), Some(busybox)) = (
            env_path("FABER_TEST_RUNSC"),
            env_path("FABER_TEST_HELPER"),
            env_path("FABER_TEST_BUSYBOX"),
        ) else {
            panic!("set FABER_TEST_RUNSC, FABER_TEST_HELPER and FABER_TEST_BUSYBOX");
        };
        let dir = Temp::new("compute");
        let mut config = config(&dir);
        let bin = config.rootfs.join("usr/bin");
        std::fs::copy(&busybox, bin.join("busybox")).unwrap();
        std::fs::set_permissions(bin.join("busybox"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        for name in ["sh", "echo", "cat", "ls"] {
            symlink("busybox", bin.join(name)).unwrap();
        }
        config.runsc = Some(runsc);
        config.helper = Some(helper);
        config.shell = "/bin/sh".to_owned();
        // Safety: geteuid has no preconditions.
        config.rootless = unsafe { libc::geteuid() } != 0;
        let scratch = Sandboxed::with_config(config);
        let key = key();

        let out = exec(&scratch, &key, "echo before > /root/notes", false).await;
        assert!(out.starts_with("mode: virtual"), "{out}");

        let out = exec(&scratch, &key, "cat /root/notes", true).await;
        assert!(out.starts_with("mode: compute"), "{out}");
        assert!(out.contains("before"), "{out}");

        // Promotion is sticky until idle teardown.
        let out = exec(&scratch, &key, "echo after", false).await;
        assert!(out.starts_with("mode: compute"), "{out}");

        scratch.delete(&key.session).await;
    }
}
