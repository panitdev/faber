//! A session's scratch, and the manager that owns every session's.
//!
//! A [`Session`] is one [`environment::Target`]: a single
//! [`Machine`](environment::Machine) over a [`Spawn`] and a [`Files`] that
//! dispatch on the session's mode. Because the machine outlives every mode
//! switch, process ids never restart, a finished process's output survives
//! idle teardown, and every result is shaped by the same code whichever mode
//! produced it. The mode itself is reported out of band, by [`Session::mode`]
//! and [`Session::prepare`].
//!
//! A session owns no storage. What it sees is its mount table: its project's
//! workspace and `/tmp`, and the plugins' mounts under `/mnt`. Both modes see
//! the same table, so the workspace and `/tmp` are the same directories in
//! either, and they are the only places a write is kept. A write anywhere
//! else goes to an ephemeral layer — the virtual shell's layer over the base
//! image, or the container's own root in compute — which promotion and idle
//! drop. `$TMPDIR` is the session's own directory in `/tmp`, emptied at idle;
//! the rest of `/tmp` is shared across the project's sessions and swept of
//! what has gone untouched for a day.
//!
//! Virtual is where a session starts and where it returns after idle
//! teardown. A call promotes it to compute when [`decide`] says so, when the
//! agent asks for compute, or when it starts a process — always before
//! anything of the call runs. A virtual run that fails is a failed run;
//! nothing is run again in the container after the fact. Promotion is sticky
//! until idle teardown.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use environment::files::{Confined, DirEntry, Files};
use environment::spawn::{Proc, Run, Sink, Source, Spawn};
use environment::{
    Blob, Blobs, Capability, Chunk, Cursor, Denial, Edit, Exec, Exit, Fault, Label, Listing,
    Machine, Manifest, Outcome, Posture, ProcId, Process, Reachability, Root, RootedPath, Scope,
    Signal, Span, Stat, Target, Window,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedRwLockReadGuard, RwLock, watch};

use crate::compute::{Container, ContainerFiles, Names, PrivateTmp, Tenantd, TenantdConfig};
use crate::decide::{Decision, decide};
use crate::files::VirtualFiles;
use crate::layer::Layer;
use crate::mount::{ErrorLog, MountBackend, MountError, MountTable};
use crate::quota::{Quota, Usage};
use crate::shell::{self, VirtualCommand};
use crate::vfs::{Bind, ScratchFs, TOOLSTORE};
use crate::volume::{LocalVolume, Volume};

/// Which of scratch's two modes ran, or will run, a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Virtual,
    Compute,
}

/// What a call is about to do, so the mode is settled before any of it runs.
#[derive(Clone, Copy, Debug)]
pub enum Call<'a> {
    Exec {
        command: &'a str,
        compute: bool,
    },
    /// `process start`: always compute, the only mode that keeps a process.
    Start,
    Process,
    Read,
    Patch,
}

/// Everything an operator sets.
#[derive(Clone, Debug)]
pub struct ScratchConfig {
    /// Sessions' ephemeral layers live under `<root>/sessions/<session>`;
    /// without tenantd, projects' volumes under `<root>/projects/<project>`.
    pub root: PathBuf,
    /// The base image, exported as a directory: what virtual mode runs
    /// over. Read-only to scratch. Compute runs the same image from
    /// [`TenantdConfig::image`].
    pub rootfs: PathBuf,
    /// Served read-only at `/opt/toolstore` in virtual mode. Compute sees
    /// what the image has there, so with tenantd the image should carry it.
    pub toolstore: Option<PathBuf>,
    /// Without tenantd there is no compute: a call that needs it fails, and
    /// says why, and the project's volumes are directories under `root`.
    pub tenantd: Option<TenantdConfig>,
    /// Bytes one user's sessions may hold in their ephemeral layers in
    /// virtual mode. With tenantd, the user's tenant limit bounds the rest.
    pub quota: Option<u64>,
    /// How long a session sits unused before its container is stopped and
    /// its ephemeral state dropped.
    pub idle_timeout: Duration,
    /// The environment every command starts with, in both modes.
    pub env: Vec<(String, String)>,
    /// `$HOME` in the image.
    pub home: String,
    /// Where the project's workspace is, in both modes: where tenantd's
    /// profile mounts its `workspace` slot.
    pub workspace: String,
    /// The shell `exec` runs through in compute; the virtual shell is bash.
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

/// Where the project's shared `/tmp` is, in both modes.
pub const TMP: &str = "/tmp";

impl ScratchConfig {
    /// Defaults matching the Debian base image.
    pub fn new(root: impl Into<PathBuf>, rootfs: impl Into<PathBuf>) -> Self {
        ScratchConfig {
            root: root.into(),
            rootfs: rootfs.into(),
            toolstore: None,
            tenantd: None,
            quota: None,
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
            workspace: "/workspace".to_owned(),
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

/// The refusal a call gets when scratch cannot run it, which has run nothing.
fn refused(reason: impl Into<String>) -> Fault {
    Fault::Denied(Denial::Malformed {
        what: "scratch".into(),
        reason: reason.into(),
    })
}

enum State {
    Virtual,
    Compute(Arc<Container>),
}

/// What the dispatching spawn and files share with the session.
struct Inner {
    id: String,
    user: String,
    config: Arc<ScratchConfig>,
    tenantd: Option<Arc<Tenantd>>,
    /// The session's objects in tenantd, when there is one.
    names: Option<Names>,
    /// The environment every command of this session starts with: the
    /// configured one and `$TMPDIR`.
    env: Vec<(String, String)>,
    private_tmp: PrivateTmp,
    quota: Arc<Quota>,
    usage: Usage,
    layer: Arc<Layer>,
    fs: Arc<ScratchFs>,
    mounts: Arc<MountTable>,
    errors: Arc<ErrorLog>,
    /// Virtual work holds a read guard for as long as it touches the layer;
    /// switching modes takes the write guard, so a promotion never drops a
    /// layer something is still writing.
    state: Arc<RwLock<State>>,
}

impl Inner {
    /// Switches to compute, starting the session's container. Refuses,
    /// having started nothing, when there is no tenantd or the user is out of
    /// quota. Only the mount table carries over: the ephemeral layer is
    /// dropped, as the container's own root will be at idle.
    async fn promote(&self, why: &str) -> Result<Arc<Container>, Fault> {
        let mut state = self.state.write().await;
        if let State::Compute(container) = &*state {
            return Ok(Arc::clone(container));
        }
        self.quota
            .check(&self.user)
            .map_err(|exceeded| refused(format!("this needs compute ({why}), but {exceeded}")))?;
        let (Some(tenantd), Some(names)) = (&self.tenantd, &self.names) else {
            return Err(refused(format!(
                "this needs compute ({why}), and this Faber server has no sandbox runtime"
            )));
        };
        let container = tenantd
            .start(
                names.clone(),
                &self.config.shell,
                self.env.clone(),
                self.private_tmp.clone(),
                Arc::clone(&self.mounts),
                Arc::clone(&self.errors),
            )
            .await?;
        self.drop_layer();
        tracing::info!(session = %self.id, reason = %why, "scratch promoted to compute");
        *state = State::Compute(Arc::new(container));
        match &*state {
            State::Compute(container) => Ok(Arc::clone(container)),
            State::Virtual => unreachable!("set above"),
        }
    }

    /// Stops the container, dropping its root, and returns to virtual mode.
    async fn demote(&self) {
        let mut state = self.state.write().await;
        if let State::Compute(container) = &*state {
            container.stop().await;
            tracing::info!(session = %self.id, "scratch stopped its idle container");
        }
        *state = State::Virtual;
    }

    fn drop_layer(&self) {
        if let Err(error) = self.layer.clear() {
            tracing::warn!(session = %self.id, %error, "scratch could not drop its layer");
        }
        self.usage.set(self.layer.bytes());
    }

    /// Empties the session's own directory in `/tmp`.
    async fn reset_private_tmp(&self) {
        if let (Some(tenantd), Some(names)) = (&self.tenantd, &self.names) {
            let _ = tenantd
                .prune(
                    &names.tenant,
                    &names.tmp,
                    &crate::compute::private_dir(&self.id),
                    Duration::ZERO,
                )
                .await;
        } else {
            let _ = self
                .private_tmp
                .volume
                .remove_all(&self.private_tmp.dir)
                .await;
        }
        let _ = self
            .private_tmp
            .volume
            .mkdir(&self.private_tmp.dir, None, true)
            .await;
    }

    /// What idle drops, in either mode: the container, the ephemeral layer
    /// and `$TMPDIR`'s contents.
    async fn expire(&self) {
        self.demote().await;
        {
            let _state = self.state.write().await;
            self.drop_layer();
        }
        self.reset_private_tmp().await;
    }

    fn mode(state: &State) -> Mode {
        match state {
            State::Virtual => Mode::Virtual,
            State::Compute(_) => Mode::Compute,
        }
    }
}

/// Runs each command in the session's current mode, promoting first when the
/// command cannot run virtually.
struct SessionSpawn {
    inner: Arc<Inner>,
}

/// The command a run carries: `[shell, "-c", command]`.
fn command_of(run: &Run) -> &str {
    match run.argv.as_slice() {
        [_, flag, command] if flag == "-c" => command,
        _ => "",
    }
}

#[async_trait]
impl Spawn for SessionSpawn {
    async fn spawn(&self, run: Run) -> Result<Box<dyn Proc>, Fault> {
        let state = Arc::clone(&self.inner.state).read_owned().await;
        let container = match &*state {
            State::Compute(container) => Arc::clone(container),
            State::Virtual => match decide(command_of(&run)) {
                Decision::Virtual => {
                    return Ok(VirtualProc::spawn(Arc::clone(&self.inner), state, run));
                }
                Decision::Promote(why) => {
                    drop(state);
                    self.inner.promote(&why).await?
                }
            },
        };
        container.spawn(run).await
    }
}

/// A command running in the virtual shell, shaped like a process so the
/// machine above treats it like any other.
struct VirtualProc {
    stdin: Option<Sink>,
    stdout: Option<Source>,
    stderr: Option<Source>,
    done: watch::Receiver<Option<Result<Outcome, Fault>>>,
    cancel: watch::Sender<bool>,
}

/// How much of a stream the pipe between the run and the machine buffers.
const PIPE: usize = 64 * 1024;

/// The virtual shell's own time limit, from start to finish, reading its
/// stdin included. The machine applies the call's timeout by cancelling the
/// run; this backstops a run nobody waits on. A virtual run holds the
/// session's mode for as long as it lasts, so a promotion waiting behind it
/// — and every call queued behind that — waits at most this long. Virtual
/// commands are file work and probes; anything long belongs in compute.
const VIRTUAL_LIMIT: Duration = Duration::from_secs(10 * 60);

impl VirtualProc {
    fn spawn(inner: Arc<Inner>, guard: OwnedRwLockReadGuard<State>, run: Run) -> Box<dyn Proc> {
        let (stdin, mut input) = tokio::io::duplex(PIPE);
        let (mut out, stdout) = tokio::io::duplex(PIPE);
        let (mut err, stderr) = tokio::io::duplex(PIPE);
        let (done_tx, done) = watch::channel(None);
        let (cancel, cancelled) = watch::channel(false);

        tokio::spawn(async move {
            let started = Instant::now();
            let mut body = Vec::new();
            if tokio::time::timeout(VIRTUAL_LIMIT, input.read_to_end(&mut body))
                .await
                .is_err()
            {
                drop(guard);
                let _ = done_tx.send(Some(Ok(Outcome::TimedOut)));
                return;
            }
            let mut env = inner.env.clone();
            env.extend(run.env.iter().cloned());
            let result = shell::run(
                Arc::clone(&inner.fs),
                VirtualCommand {
                    script: command_of(&run),
                    cwd: &run.cwd,
                    env: &env,
                    stdin: Some(body),
                    timeout: VIRTUAL_LIMIT.saturating_sub(started.elapsed()),
                },
                cancelled.clone(),
            )
            .await;
            drop(guard);

            let _ = out.write_all(&result.stdout).await;
            let _ = err.write_all(&result.stderr).await;
            drop(out);
            drop(err);
            let _ = done_tx.send(Some(Ok(result.outcome)));
        });

        Box::new(VirtualProc {
            stdin: Some(Box::new(stdin)),
            stdout: Some(Box::new(stdout)),
            stderr: Some(Box::new(stderr)),
            done,
            cancel,
        })
    }
}

#[async_trait]
impl Proc for VirtualProc {
    fn stdin(&mut self) -> Option<Sink> {
        self.stdin.take()
    }

    fn stdout(&mut self) -> Option<Source> {
        self.stdout.take()
    }

    fn stderr(&mut self) -> Option<Source> {
        self.stderr.take()
    }

    async fn wait(&mut self) -> Result<Outcome, Fault> {
        loop {
            if let Some(result) = self.done.borrow().clone() {
                return result;
            }
            if self.done.changed().await.is_err() {
                return Err(Fault::Unreachable("the virtual shell stopped".to_owned()));
            }
        }
    }

    async fn try_wait(&mut self) -> Result<Option<Outcome>, Fault> {
        self.done.borrow().clone().transpose()
    }

    async fn signal(&mut self, _signal: Signal) -> Result<(), Fault> {
        // The virtual shell has no signal handlers to run: any signal ends it.
        let _ = self.cancel.send(true);
        Ok(())
    }
}

/// Files in the session's current mode.
struct SessionFiles {
    inner: Arc<Inner>,
}

impl SessionFiles {
    async fn with<T>(
        &self,
        virtual_: impl AsyncFnOnce(&VirtualFiles) -> Result<T, Fault>,
        compute: impl AsyncFnOnce(&ContainerFiles) -> Result<T, Fault>,
    ) -> Result<T, Fault> {
        let state = self.inner.state.read().await;
        match &*state {
            State::Virtual => {
                virtual_(&VirtualFiles {
                    fs: Arc::clone(&self.inner.fs),
                })
                .await
            }
            State::Compute(container) => {
                compute(&ContainerFiles {
                    container: Arc::clone(container),
                })
                .await
            }
        }
    }
}

#[async_trait]
impl Files for SessionFiles {
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault> {
        self.with(
            async |files| files.confine(path).await,
            async |files| files.confine(path).await,
        )
        .await
    }

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        self.with(
            async |files| files.fetch(path).await,
            async |files| files.fetch(path).await,
        )
        .await
    }

    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        self.with(
            async |files| files.store(path, body).await,
            async |files| files.store(path, body).await,
        )
        .await
    }

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault> {
        self.with(
            async |files| files.remove(path).await,
            async |files| files.remove(path).await,
        )
        .await
    }

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault> {
        self.with(
            async |files| files.rename(from, to).await,
            async |files| files.rename(from, to).await,
        )
        .await
    }

    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault> {
        self.with(
            async |files| files.enumerate(dir).await,
            async |files| files.enumerate(dir).await,
        )
        .await
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
        );
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

    /// The mode the session is in now. Read after a call, it is the mode
    /// that call ran in: promotion is sticky, and only idle teardown — which
    /// waits for calls in flight — returns to virtual.
    pub async fn mode(&self) -> Mode {
        Inner::mode(&*self.inner.state.read().await)
    }

    fn call(&self) -> Busy<'_> {
        self.busy.fetch_add(1, Ordering::SeqCst);
        *self.last_call.lock().expect("clock poisoned") = Instant::now();
        self.expired.store(false, Ordering::SeqCst);
        Busy(&self.busy)
    }

    /// Settles the mode a call will run in before any of it runs: promotes
    /// for `compute`, a command off the virtual allowlist, and `process
    /// start`. An error means nothing ran — no runtime, or the user's quota
    /// is used up.
    pub async fn prepare(&self, call: Call<'_>) -> Result<Mode, Fault> {
        let _busy = self.call();
        let why = match call {
            Call::Exec { compute: true, .. } => Some("the call asked for compute".to_owned()),
            Call::Exec { command, .. } => match decide(command) {
                Decision::Virtual => None,
                Decision::Promote(why) => Some(why),
            },
            Call::Start => Some("process start always runs in compute".to_owned()),
            Call::Process | Call::Read | Call::Patch => None,
        };
        match why {
            Some(why) => {
                self.inner.promote(&why).await?;
                Ok(Mode::Compute)
            }
            None => Ok(self.mode().await),
        }
    }

    /// `exec` with the agent's `compute` flag, returning the mode it ran in.
    pub async fn run(&self, req: Exec, compute: bool) -> Result<(Exit, Mode), Fault> {
        self.prepare(Call::Exec {
            command: &req.command,
            compute,
        })
        .await?;
        let exit = self.exec(req).await?;
        Ok((exit, self.mode().await))
    }

    /// Starts a command the way `exec` does: in the session's current mode,
    /// promoting first only when the command itself needs compute. A command
    /// still running when its caller stops waiting is a process like any
    /// other. `process start` goes through [`Target::start`] instead, which
    /// always promotes.
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
    /// same name. Visible to the next call in either mode, and to a running
    /// container at once.
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

    /// Bytes this session's ephemeral layer holds, as last counted.
    pub fn bytes(&self) -> u64 {
        self.inner.usage.bytes()
    }

    /// Lets the session go idle if it has been unused for `idle` with no
    /// call in flight and no managed process (one from `process start`)
    /// running: its container stops, with whatever else still runs in it —
    /// what an `exec` left behind — and its ephemeral layer and `$TMPDIR`
    /// are emptied. Returns whether it did.
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
        let running: Vec<ProcId> = match self.mode().await {
            Mode::Virtual => Vec::new(),
            Mode::Compute => self
                .machine
                .processes()
                .await
                .map(|processes| {
                    processes
                        .into_iter()
                        .filter(|process| process.outcome.is_none())
                        .map(|process| process.id)
                        .collect()
                })
                .unwrap_or_default(),
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
        self.inner.demote().await;
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
        self.inner
            .promote("process start always runs in compute")
            .await?;
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
    tenantd: Option<Arc<Tenantd>>,
    quota: Arc<Quota>,
    sessions: tokio::sync::Mutex<HashMap<String, Arc<Session>>>,
    last_sweep: Mutex<Option<Instant>>,
}

/// A session or project id becomes a directory name and part of tenantd's
/// names, so it is held to what both accept. Faber's are UUIDs.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Where a session's records are kept: who owns it, and its project.
const OWNER: &str = "owner";
const PROJECT: &str = "project";

impl ScratchManager {
    pub fn new(config: ScratchConfig) -> Result<Self, ScratchError> {
        if !config.rootfs.is_dir() {
            return Err(ScratchError::Unavailable(format!(
                "the base image `{}` is not a directory",
                config.rootfs.display()
            )));
        }
        let storage = |error: std::io::Error| ScratchError::Storage(error.to_string());
        std::fs::create_dir_all(config.root.join("sessions")).map_err(storage)?;
        if config.tenantd.is_none() {
            std::fs::create_dir_all(config.root.join("projects")).map_err(storage)?;
        }
        let quota = Arc::new(Quota::new(config.quota));
        let tenantd = config
            .tenantd
            .clone()
            .map(|tenantd| Arc::new(Tenantd::new(tenantd)));
        Ok(ScratchManager {
            config: Arc::new(config),
            tenantd,
            quota,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(None),
        })
    }

    pub fn config(&self) -> &ScratchConfig {
        &self.config
    }

    pub fn quota(&self) -> &Arc<Quota> {
        &self.quota
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.config.root.join("sessions").join(id)
    }

    /// Deletes what an earlier Faber process left running in tenantd, and
    /// the ephemeral layers it left on disk. Call once at startup, before any
    /// session is opened.
    pub async fn recover(&self) {
        if let Some(tenantd) = &self.tenantd {
            tenantd.recover().await;
        }
        let Ok(entries) = std::fs::read_dir(self.config.root.join("sessions")) else {
            return;
        };
        for entry in entries.flatten() {
            let _ = std::fs::remove_dir_all(entry.path().join("layer"));
        }
    }

    /// The project's workspace and `/tmp`: in tenantd when there is one,
    /// else on this host.
    async fn volumes(
        &self,
        project: &str,
        names: Option<&Names>,
    ) -> Result<(Arc<dyn Volume>, Arc<dyn Volume>), ScratchError> {
        match (&self.tenantd, names) {
            (Some(tenantd), Some(names)) => tenantd
                .ensure_volumes(names)
                .await
                .map_err(|fault| ScratchError::Unavailable(fault.to_string())),
            _ => {
                let dir = self.config.root.join("projects").join(project);
                let (workspace, tmp) = (dir.join("workspace"), dir.join("tmp"));
                for volume in [&workspace, &tmp] {
                    std::fs::create_dir_all(volume)
                        .map_err(|error| ScratchError::Storage(error.to_string()))?;
                }
                Ok((
                    Arc::new(LocalVolume::new(workspace)),
                    Arc::new(LocalVolume::new(tmp)),
                ))
            }
        }
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
        let dir = self.dir(id);
        let storage = |error: std::io::Error| ScratchError::Storage(error.to_string());
        std::fs::create_dir_all(&dir).map_err(storage)?;
        // The first owner and project stay: the quota a session counts
        // against and the volumes it sees never move.
        let recorded = |name: &str, given: &str| -> Result<String, ScratchError> {
            match std::fs::read_to_string(dir.join(name)) {
                Ok(kept) => Ok(kept.trim().to_owned()),
                Err(_) => {
                    std::fs::write(dir.join(name), given).map_err(storage)?;
                    Ok(given.to_owned())
                }
            }
        };
        let owner = recorded(OWNER, user)?;
        let project = recorded(PROJECT, project)?;
        if !valid_id(&project) {
            return Err(ScratchError::Storage(format!(
                "`{project}` is not a project id scratch accepts"
            )));
        }
        let names = match &self.tenantd {
            Some(_) => Some(Names::new(&owner, &project, id).ok_or_else(|| {
                ScratchError::Storage(format!(
                    "session `{id}` of `{owner}` in `{project}` has no name tenantd accepts"
                ))
            })?),
            None => None,
        };
        let (workspace, tmp) = self.volumes(&project, names.as_ref()).await?;

        let private = crate::compute::private_dir(id);
        let private_tmp = PrivateTmp {
            volume: Arc::clone(&tmp),
            dir: format!("/{private}"),
            path: format!("{TMP}/{private}"),
        };
        private_tmp
            .volume
            .mkdir(&private_tmp.dir, None, true)
            .await
            .map_err(storage)?;
        let mut env = self.config.env.clone();
        env.retain(|(key, _)| key != "TMPDIR");
        env.push(("TMPDIR".to_owned(), private_tmp.path.clone()));

        // Whatever a layer holds from before this process is past idle.
        let layer_dir = dir.join("layer");
        let _ = std::fs::remove_dir_all(&layer_dir);
        let layer = Arc::new(Layer::open(layer_dir).map_err(storage)?);
        let usage = self.quota.track(&owner, id, 0);
        let mounts = Arc::new(MountTable::default());
        let errors = Arc::new(ErrorLog::default());
        let mut binds = vec![
            Bind {
                at: self.config.workspace.clone(),
                volume: workspace,
                read_only: false,
            },
            Bind {
                at: TMP.to_owned(),
                volume: tmp,
                read_only: false,
            },
        ];
        if let Some(toolstore) = &self.config.toolstore {
            binds.push(Bind {
                at: TOOLSTORE.to_owned(),
                volume: Arc::new(LocalVolume::new(toolstore.clone())),
                read_only: true,
            });
        }
        let fs = Arc::new(ScratchFs::new(
            self.config.rootfs.clone(),
            binds,
            Arc::clone(&layer),
            Arc::clone(&mounts),
            Arc::clone(&errors),
            usage.clone(),
        ));
        let inner = Arc::new(Inner {
            id: id.to_owned(),
            user: owner,
            config: Arc::clone(&self.config),
            tenantd: self.tenantd.clone(),
            names,
            env,
            private_tmp,
            quota: Arc::clone(&self.quota),
            usage,
            layer,
            fs,
            mounts,
            errors,
            state: Arc::new(RwLock::new(State::Virtual)),
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
        match &self.tenantd {
            Some(tenantd) => {
                let volumes = match tenantd.tmp_volumes().await {
                    Ok(volumes) => volumes,
                    Err(fault) => {
                        tracing::warn!(%fault, "scratch could not sweep /tmp");
                        return;
                    }
                };
                for (tenant, volume, files) in volumes {
                    crate::compute::sweep_tmp(files.as_ref(), |name| {
                        let (tenant, volume) = (tenant.clone(), volume.clone());
                        async move {
                            let _ = tenantd.prune(&tenant, &volume, &name, older_than).await;
                        }
                    })
                    .await;
                }
            }
            None => {
                let Ok(projects) = std::fs::read_dir(self.config.root.join("projects")) else {
                    return;
                };
                for project in projects.flatten() {
                    let tmp = project.path().join("tmp");
                    crate::compute::sweep_tmp(&LocalVolume::new(&tmp), |name| {
                        let path = tmp.join(name);
                        async move { prune_local(&path, older_than) }
                    })
                    .await;
                }
            }
        }
    }

    /// Deletes a session's scratch: its container, its ephemeral layer and
    /// its `$TMPDIR`. Its project's volumes stay; they are the project's.
    pub async fn delete_session(&self, id: &str) {
        if !valid_id(id) {
            return;
        }
        let removed = self.sessions.lock().await.remove(id);
        let dir = self.dir(id);
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .map(|kept| kept.trim().to_owned())
        };
        let (owner, project) = match &removed {
            Some(session) => {
                session.kill().await;
                (Some(session.user().to_owned()), read(PROJECT))
            }
            None => (read(OWNER), read(PROJECT)),
        };
        if let (Some(owner), Some(project)) = (&owner, &project) {
            let private = crate::compute::private_dir(id);
            match (&self.tenantd, Names::new(owner, project, id)) {
                (Some(tenantd), Some(names)) => {
                    if removed.is_none() {
                        // A container this process never opened, if any.
                        Container::orphan(Arc::clone(tenantd), names.clone())
                            .stop()
                            .await;
                    }
                    let _ = tenantd
                        .prune(&names.tenant, &names.tmp, &private, Duration::ZERO)
                        .await;
                }
                _ => {
                    let tmp = self.config.root.join("projects").join(project).join("tmp");
                    let _ = tokio::fs::remove_dir_all(tmp.join(private)).await;
                }
            }
        }
        if let Some(owner) = owner {
            self.quota.forget(&owner, id);
        }
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
}

/// Removes what at or below `path` has gone untouched — accessed, modified
/// or changed — for `older_than`, never following a symlink. Directories go
/// once empty.
fn prune_local(path: &Path, older_than: Duration) {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir()
        && let Ok(entries) = std::fs::read_dir(path)
    {
        for entry in entries.flatten() {
            prune_local(&entry.path(), older_than);
        }
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    let touched = meta.atime().max(meta.mtime()).max(meta.ctime());
    if now - touched < older_than.as_secs() as i64 {
        return;
    }
    if meta.is_dir() {
        // Fails while anything newer is left in it.
        let _ = std::fs::remove_dir(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// For tests and tools: where a session's ephemeral layer is.
pub fn layer_dir(root: &Path, session: &str) -> PathBuf {
    root.join("sessions").join(session).join("layer")
}

/// For tests and tools: where a project's volumes are without tenantd.
pub fn project_dir(root: &Path, project: &str) -> PathBuf {
    root.join("projects").join(project)
}
