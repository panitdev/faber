//! A session's scratch, and the manager that owns every session's.
//!
//! A [`Session`] is one [`environment::Target`]: a single
//! [`Machine`](environment::Machine) over a [`Spawn`] and a [`Files`] that
//! reach the session's container, starting it when there is none. Because
//! the machine outlives every container, process ids never restart and a
//! finished process's output survives idle teardown.
//!
//! A session owns no storage. What it sees is its project's workspace and
//! `/tmp`, and the plugins' mounts under `/mnt`, and those are the only places
//! a write is kept. A write anywhere else goes to the container's own root,
//! which idle drops. `$TMPDIR` is the session's own directory in `/tmp`,
//! emptied at idle; the rest of `/tmp` is shared across the project's sessions
//! and swept of what has gone untouched for a day.
//!
//! A session starts with no container. Its first command or file call starts
//! one, before any of the call runs, and it runs until idle teardown.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use environment::files::{Confined, DirEntry, Files};
use environment::spawn::{Proc, Run, Spawn};
use environment::{
    Blob, Blobs, Capability, Chunk, Cursor, Denial, Edit, Exec, Exit, Fault, Label, Listing,
    Machine, Manifest, Posture, ProcId, Process, Reachability, Root, RootedPath, Scope, Signal,
    Span, Stat, Target, Window,
};
use tokio::sync::RwLock;

use crate::compute::{Container, ContainerFiles, Names, PrivateTmp, Tenantd, TenantdConfig};
use crate::mount::{ErrorLog, MountBackend, MountError, MountTable};

/// What a call needs of its session, settled before any of it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    /// Runs a command or touches files, in the container.
    Container,
    /// Looks at or signals a process already started, which needs no
    /// container to be started for it.
    Process,
}

/// Everything an operator sets.
#[derive(Clone, Debug)]
pub struct ScratchConfig {
    pub tenantd: TenantdConfig,
    /// How long a session sits unused before its container is stopped and
    /// its ephemeral state dropped.
    pub idle_timeout: Duration,
    /// The environment every command starts with.
    pub env: Vec<(String, String)>,
    /// `$HOME` in the image.
    pub home: String,
    /// The shell `exec` runs through.
    pub shell: String,
    /// Whether the profile gives containers a network, for the manifest.
    pub network: bool,
}

/// Unused this long, a session's container is stopped and its ephemeral
/// state dropped.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What in a project's `/tmp` outside every session's own directory goes
/// untouched this long is removed.
pub const TMP_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

/// How often `/tmp` is swept.
const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);

/// Where the project's shared `/tmp` is.
pub const TMP: &str = "/tmp";

impl ScratchConfig {
    /// Defaults matching the Debian base image.
    pub fn new(tenantd: TenantdConfig) -> Self {
        ScratchConfig {
            tenantd,
            idle_timeout: IDLE_TIMEOUT,
            env: vec![
                (
                    "PATH".to_owned(),
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
                ),
                ("HOME".to_owned(), "/root".to_owned()),
                ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ],
            home: "/root".to_owned(),
            shell: "/bin/bash".to_owned(),
            network: false,
        }
    }
}

/// Why scratch could not give a session at all.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ScratchError {
    #[error("scratch is not available on this Faber server: {0}")]
    Unavailable(String),
    #[error("`{0}` is not a session id scratch accepts")]
    InvalidSession(String),
    #[error("scratch storage failed: {0}")]
    Storage(String),
}

/// What the spawn and files share with the session.
struct Inner {
    id: String,
    user: String,
    config: Arc<ScratchConfig>,
    tenantd: Arc<Tenantd>,
    /// The session's objects in tenantd.
    names: Names,
    /// The environment every command of this session starts with: the
    /// configured one and `$TMPDIR`.
    env: Vec<(String, String)>,
    private_tmp: PrivateTmp,
    mounts: Arc<MountTable>,
    errors: Arc<ErrorLog>,
    /// The session's container, when it has one. Held exclusively while one
    /// starts or stops, so a session never has two.
    container: RwLock<Option<Arc<Container>>>,
}

impl Inner {
    /// The session's container, started if there is none. Refuses, having
    /// run nothing, when tenantd cannot start it.
    async fn container(&self) -> Result<Arc<Container>, Fault> {
        if let Some(container) = &*self.container.read().await
            && !container.wedged()
        {
            return Ok(Arc::clone(container));
        }
        let mut slot = self.container.write().await;
        if let Some(container) = &*slot
            && !container.wedged()
        {
            return Ok(Arc::clone(container));
        }
        if let Some(wedged) = slot.take() {
            // tenantd stopped answering for it, and it is being deleted. A
            // fresh one waits for that: its delete would otherwise race this
            // start, and the release after it could destroy the sandbox.
            wedged.stop().await;
            tracing::info!(session = %self.id, "scratch replaces a container tenantd stopped answering for");
        }
        let container = Arc::new(
            self.tenantd
                .start(
                    self.names.clone(),
                    &self.config.shell,
                    self.env.clone(),
                    self.private_tmp.clone(),
                    Arc::clone(&self.mounts),
                    Arc::clone(&self.errors),
                )
                .await?,
        );
        tracing::info!(session = %self.id, "scratch started its container");
        *slot = Some(Arc::clone(&container));
        Ok(container)
    }

    /// Stops the container, dropping its root.
    async fn stop(&self) {
        if let Some(container) = self.container.write().await.take() {
            container.stop().await;
            tracing::info!(session = %self.id, "scratch stopped its container");
        }
    }

    /// Empties the session's own directory in `/tmp`.
    async fn reset_private_tmp(&self) {
        if let Err(error) = self
            .tenantd
            .prune(
                &self.names.tenant,
                &self.names.tmp,
                &crate::compute::private_dir(&self.id),
                Duration::ZERO,
            )
            .await
        {
            tracing::warn!(session = %self.id, %error, "could not empty a session's /tmp");
        }
        let _ = self
            .private_tmp
            .volume
            .mkdir(&self.private_tmp.dir, None, true)
            .await;
    }

    /// What idle drops: the container and `$TMPDIR`'s contents.
    async fn expire(&self) {
        self.stop().await;
        self.reset_private_tmp().await;
    }
}

/// Runs each command in the session's container.
struct SessionSpawn {
    inner: Arc<Inner>,
}

#[async_trait]
impl Spawn for SessionSpawn {
    async fn spawn(&self, run: Run) -> Result<Box<dyn Proc>, Fault> {
        self.inner.container().await?.spawn(run).await
    }
}

/// Files in the session's container.
struct SessionFiles {
    inner: Arc<Inner>,
}

impl SessionFiles {
    async fn files(&self) -> Result<ContainerFiles, Fault> {
        Ok(ContainerFiles {
            container: self.inner.container().await?,
        })
    }
}

#[async_trait]
impl Files for SessionFiles {
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault> {
        self.files().await?.confine(path).await
    }

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        self.files().await?.fetch(path).await
    }

    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        self.files().await?.store(path, body).await
    }

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault> {
        self.files().await?.remove(path).await
    }

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault> {
        self.files().await?.rename(from, to).await
    }

    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault> {
        self.files().await?.enumerate(dir).await
    }
}

/// One session's scratch.
pub struct Session {
    inner: Arc<Inner>,
    machine: Machine,
    /// Calls in flight; teardown waits for none.
    busy: AtomicUsize,
    last_call: Mutex<Instant>,
    /// Idle has dropped the session's ephemeral state since its last call.
    expired: AtomicBool,
    /// Processes started with `process start`: while one runs, the session
    /// is not idle.
    managed: Mutex<BTreeSet<ProcId>>,
    /// Processes idle teardown stopped while they ran.
    stopped: Mutex<BTreeSet<ProcId>>,
}

/// Counts a call in flight for as long as it lives.
struct Busy<'a>(&'a AtomicUsize);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Session {
    fn new(inner: Arc<Inner>, blobs: Arc<dyn Blobs>) -> Self {
        let label = Label("scratch".to_owned());
        let manifest = Manifest {
            agent_env: BTreeMap::from([
                ("FABER_AGENT".to_owned(), "1".to_owned()),
                ("FABER_TARGET".to_owned(), label.0.clone()),
            ]),
            label,
            os: "linux".to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            shell: inner.config.shell.clone(),
            root: Root::new("/").expect("/ is a root"),
            home: inner.config.home.clone(),
            tools: BTreeMap::new(),
            capabilities: BTreeSet::from([
                Capability::Exec,
                Capability::Background,
                Capability::Stdin,
                Capability::Read,
                Capability::Write,
                Capability::Edit,
                Capability::Patch,
                Capability::List,
            ]),
            scope: Scope::Container,
            network: if inner.config.network {
                Reachability::Reachable
            } else {
                Reachability::Unreachable
            },
            posture: Posture::Enforced,
            login_shell_sourced: false,
            probed_at: SystemTime::now(),
        };
        let machine = Machine::new(
            manifest,
            Arc::new(SessionSpawn {
                inner: Arc::clone(&inner),
            }),
            Arc::new(SessionFiles {
                inner: Arc::clone(&inner),
            }),
            blobs,
        )
        // Its output streams are in-memory pipes the container's pumps fill
        // on that runtime too.
        .with_runtime(inner.tenantd.runtime().clone());
        Session {
            inner,
            machine,
            busy: AtomicUsize::new(0),
            last_call: Mutex::new(Instant::now()),
            expired: AtomicBool::new(false),
            managed: Mutex::new(BTreeSet::new()),
            stopped: Mutex::new(BTreeSet::new()),
        }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn user(&self) -> &str {
        &self.inner.user
    }

    /// `$TMPDIR` in scratch: the session's own directory in `/tmp`.
    pub fn tmpdir(&self) -> &str {
        &self.inner.private_tmp.path
    }

    /// Whether the session has a container now.
    pub async fn running(&self) -> bool {
        self.inner.container.read().await.is_some()
    }

    fn call(&self) -> Busy<'_> {
        self.busy.fetch_add(1, Ordering::SeqCst);
        *self.last_call.lock().expect("clock poisoned") = Instant::now();
        self.expired.store(false, Ordering::SeqCst);
        Busy(&self.busy)
    }

    /// Starts the container a call needs before any of the call runs. An
    /// error means nothing ran.
    pub async fn prepare(&self, call: Call) -> Result<(), Fault> {
        let _busy = self.call();
        if call == Call::Container {
            self.inner.container().await?;
        }
        Ok(())
    }

    /// Starts a command the way `exec` does. A command still running when
    /// its caller stops waiting is a process like any other. `process start`
    /// goes through [`Target::start`] instead, which also keeps the session
    /// from going idle while it runs.
    pub async fn begin(&self, req: Exec) -> Result<ProcId, Fault> {
        let _busy = self.call();
        self.machine.start(req).await
    }

    /// Marks `id` as started with `process start`, for a caller that started
    /// it through [`begin`](Self::begin). While it runs, idle teardown waits.
    pub fn manage(&self, id: ProcId) {
        self.managed.lock().expect("managed poisoned").insert(id);
    }

    /// Mounts `backend` at `/mnt/<plugin>/<name>`, replacing a mount of the
    /// same name. Visible to the next call, and to a running container at
    /// once.
    pub fn mount(
        &self,
        plugin: &str,
        name: &str,
        read_only: bool,
        backend: Arc<dyn MountBackend>,
    ) -> Result<String, MountError> {
        self.inner.mounts.mount(plugin, name, read_only, backend)
    }

    pub fn unmount(&self, plugin: &str, name: &str) -> Result<(), MountError> {
        self.inner.mounts.unmount(plugin, name)
    }

    /// Paths of the session's mounts.
    pub fn mounts(&self) -> Vec<String> {
        self.inner
            .mounts
            .list()
            .into_iter()
            .map(|(plugin, name, _)| plugin::environments::mount_path(&plugin, &name))
            .collect()
    }

    /// Lets the session go idle if it has been unused for `idle` with no
    /// call in flight and no managed process (one from `process start`)
    /// running: its container stops, with whatever else still runs in it —
    /// what an `exec` left behind — and `$TMPDIR` is emptied. Returns whether
    /// it did.
    pub async fn teardown_idle(&self, idle: Duration) -> bool {
        if self.busy.load(Ordering::SeqCst) > 0
            || self.last_call.lock().expect("clock poisoned").elapsed() < idle
            || self.expired.load(Ordering::SeqCst)
        {
            return false;
        }
        let managed = self.managed.lock().expect("managed poisoned").clone();
        match self.machine.processes().await {
            Ok(processes)
                if !processes
                    .iter()
                    .any(|p| p.outcome.is_none() && managed.contains(&p.id)) => {}
            _ => return false,
        }
        self.teardown().await;
        true
    }

    /// Goes idle now. Background processes still running are stopped with
    /// the container; a later call on one of them says so.
    pub async fn teardown(&self) {
        let running: Vec<ProcId> = if self.running().await {
            self.machine
                .processes()
                .await
                .map(|processes| {
                    processes
                        .into_iter()
                        .filter(|process| process.outcome.is_none())
                        .map(|process| process.id)
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.inner.expire().await;
        self.expired.store(true, Ordering::SeqCst);
        self.stopped
            .lock()
            .expect("stopped poisoned")
            .extend(running);
    }

    /// Refuses a call on a process idle teardown stopped.
    fn live(&self, id: ProcId) -> Result<(), Fault> {
        if self.stopped.lock().expect("stopped poisoned").contains(&id) {
            return Err(Fault::Denied(Denial::Stopped(id)));
        }
        Ok(())
    }

    /// Stops everything, for deletion.
    async fn kill(&self) {
        self.inner.stop().await;
    }
}

#[async_trait]
impl Target for Session {
    fn manifest(&self) -> &Manifest {
        self.machine.manifest()
    }

    async fn exec(&self, req: Exec) -> Result<Exit, Fault> {
        let _busy = self.call();
        self.machine.exec(req).await
    }

    async fn start(&self, req: Exec) -> Result<ProcId, Fault> {
        let _busy = self.call();
        let id = self.machine.start(req).await?;
        self.manage(id);
        Ok(id)
    }

    async fn output(&self, id: ProcId, from: Cursor) -> Result<Chunk, Fault> {
        let _busy = self.call();
        self.live(id)?;
        self.machine.output(id, from).await
    }

    async fn stdin(&self, id: ProcId, body: &Blob) -> Result<(), Fault> {
        let _busy = self.call();
        self.live(id)?;
        self.machine.stdin(id, body).await
    }

    async fn close_stdin(&self, id: ProcId) -> Result<(), Fault> {
        let _busy = self.call();
        self.live(id)?;
        self.machine.close_stdin(id).await
    }

    async fn processes(&self) -> Result<Vec<Process>, Fault> {
        let _busy = self.call();
        self.machine.processes().await
    }

    async fn signal(&self, id: ProcId, signal: Signal) -> Result<(), Fault> {
        let _busy = self.call();
        self.live(id)?;
        self.machine.signal(id, signal).await
    }

    async fn read(&self, path: &RootedPath, window: Option<Window>) -> Result<Span, Fault> {
        let _busy = self.call();
        self.machine.read(path, window).await
    }

    async fn write(&self, path: &RootedPath, body: &Blob) -> Result<Stat, Fault> {
        let _busy = self.call();
        self.machine.write(path, body).await
    }

    async fn edit(&self, edit: &Edit) -> Result<Vec<Stat>, Fault> {
        let _busy = self.call();
        self.machine.edit(edit).await
    }

    async fn list(&self, path: &RootedPath, glob: Option<&str>) -> Result<Listing, Fault> {
        let _busy = self.call();
        self.machine.list(path, glob).await
    }
}

/// Every session's scratch on this server.
pub struct ScratchManager {
    config: Arc<ScratchConfig>,
    tenantd: Arc<Tenantd>,
    sessions: tokio::sync::Mutex<HashMap<String, Arc<Session>>>,
    last_sweep: Mutex<Option<Instant>>,
}

/// A session or project id becomes part of tenantd's names, so it is held to
/// what tenantd accepts. Faber's are UUIDs.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl ScratchManager {
    pub fn new(config: ScratchConfig) -> Self {
        let tenantd = Arc::new(Tenantd::new(config.tenantd.clone()));
        ScratchManager {
            config: Arc::new(config),
            tenantd,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &ScratchConfig {
        &self.config
    }

    /// Deletes what an earlier Faber process left running in tenantd. Call
    /// once at startup, before any session is opened.
    pub async fn recover(&self) {
        self.tenantd.recover().await;
    }

    /// The session's scratch, opened on first use. `blobs` is where its
    /// results are stored, fixed when the session is first opened.
    pub async fn session(
        &self,
        id: &str,
        user: &str,
        project: &str,
        blobs: Arc<dyn Blobs>,
    ) -> Result<Arc<Session>, ScratchError> {
        if !valid_id(id) {
            return Err(ScratchError::InvalidSession(id.to_owned()));
        }
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(id) {
            return Ok(Arc::clone(session));
        }
        if !valid_id(project) {
            return Err(ScratchError::Storage(format!(
                "`{project}` is not a project id scratch accepts"
            )));
        }
        let names = Names::new(user, project, id).ok_or_else(|| {
            ScratchError::Storage(format!(
                "session `{id}` of `{user}` in `{project}` has no name tenantd accepts"
            ))
        })?;
        let (_workspace, tmp) = self
            .tenantd
            .ensure_volumes(&names)
            .await
            .map_err(|fault| ScratchError::Unavailable(fault.to_string()))?;

        let private = crate::compute::private_dir(id);
        let private_tmp = PrivateTmp {
            volume: tmp,
            dir: format!("/{private}"),
            path: format!("{TMP}/{private}"),
        };
        private_tmp
            .volume
            .mkdir(&private_tmp.dir, None, true)
            .await
            .map_err(|error| ScratchError::Storage(error.to_string()))?;
        let mut env = self.config.env.clone();
        env.retain(|(key, _)| key != "TMPDIR");
        env.push(("TMPDIR".to_owned(), private_tmp.path.clone()));

        let inner = Arc::new(Inner {
            id: id.to_owned(),
            user: user.to_owned(),
            config: Arc::clone(&self.config),
            tenantd: Arc::clone(&self.tenantd),
            names,
            env,
            private_tmp,
            mounts: Arc::new(MountTable::default()),
            errors: Arc::new(ErrorLog::default()),
            container: RwLock::new(None),
        });
        let session = Arc::new(Session::new(inner, blobs));
        sessions.insert(id.to_owned(), Arc::clone(&session));
        Ok(session)
    }

    /// Lets every session idle past the configured timeout go idle, and
    /// sweeps the projects' `/tmp`s once an hour.
    pub async fn teardown_idle(&self) {
        let sessions: Vec<Arc<Session>> = self.sessions.lock().await.values().cloned().collect();
        for session in sessions {
            session.teardown_idle(self.config.idle_timeout).await;
        }
        let due = {
            let mut last = self.last_sweep.lock().expect("sweep clock poisoned");
            let due = last.is_none_or(|at| at.elapsed() >= SWEEP_EVERY);
            if due {
                *last = Some(Instant::now());
            }
            due
        };
        if due {
            self.sweep(TMP_RETENTION).await;
        }
    }

    /// Removes what in every project's `/tmp` lies outside the sessions'
    /// own directories and has gone untouched for `older_than`.
    pub async fn sweep(&self, older_than: Duration) {
        let volumes = match self.tenantd.tmp_volumes().await {
            Ok(volumes) => volumes,
            Err(fault) => {
                tracing::warn!(%fault, "scratch could not sweep /tmp");
                return;
            }
        };
        for (tenant, volume, files) in volumes {
            crate::compute::sweep_tmp(&files, |name| {
                let (tenant, volume) = (tenant.clone(), volume.clone());
                let tenantd = Arc::clone(&self.tenantd);
                async move {
                    if let Err(error) = tenantd.prune(&tenant, &volume, &name, older_than).await {
                        tracing::warn!(%tenant, %volume, %name, %error, "could not sweep old /tmp data");
                    }
                }
            })
            .await;
        }
    }

    /// Every session with a `$TMPDIR` in tenantd, open or not. Every open
    /// session has one, so this finds what a deleted session left behind.
    /// Empty when tenantd cannot be asked: nothing is deleted on a guess.
    pub async fn stored_sessions(&self) -> Vec<String> {
        let volumes = match self.tenantd.tmp_volumes().await {
            Ok(volumes) => volumes,
            Err(fault) => {
                tracing::warn!(%fault, "scratch could not list its sessions");
                return Vec::new();
            }
        };
        let mut stored = Vec::new();
        for (tenant, volume, files) in volumes {
            match files.names("/").await {
                Ok(names) => stored.extend(
                    names
                        .iter()
                        .filter_map(|name| crate::compute::private_dir_session(name))
                        .map(str::to_owned),
                ),
                Err(error) => {
                    tracing::warn!(%tenant, %volume, %error, "scratch could not list a project's /tmp");
                }
            }
        }
        stored
    }

    /// What tenantd calls the objects of a session this process does not
    /// have open, found from its `$TMPDIR`.
    async fn stored_names(&self, id: &str) -> Option<Names> {
        let private = format!("/{}", crate::compute::private_dir(id));
        let volumes = match self.tenantd.tmp_volumes().await {
            Ok(volumes) => volumes,
            Err(fault) => {
                tracing::warn!(%fault, session = %id, "scratch could not look a session up");
                return None;
            }
        };
        for (tenant, volume, files) in volumes {
            if !files.exists(&private).await.unwrap_or(false) {
                continue;
            }
            let owner = tenant.strip_prefix("u-")?;
            let project = volume.strip_prefix("p-")?.strip_suffix("-tmp")?;
            return Names::new(owner, project, id);
        }
        None
    }

    /// Deletes a session's scratch: its container and its `$TMPDIR`. Its
    /// project's volumes stay; they are the project's.
    pub async fn delete_session(&self, id: &str) {
        if !valid_id(id) {
            return;
        }
        let removed = self.sessions.lock().await.remove(id);
        let names = match &removed {
            Some(session) => {
                session.kill().await;
                Some(session.inner.names.clone())
            }
            None => self.stored_names(id).await,
        };
        let Some(names) = names else {
            return;
        };
        if removed.is_none() {
            // A container this process never opened, if any.
            Container::orphan(Arc::clone(&self.tenantd), names.clone())
                .stop()
                .await;
        }
        let private = crate::compute::private_dir(id);
        if let Err(error) = self
            .tenantd
            .prune(&names.tenant, &names.tmp, &private, Duration::ZERO)
            .await
        {
            tracing::warn!(%private, %error, "could not delete a deleted session's /tmp");
        }
    }
}
