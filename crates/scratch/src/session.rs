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
//! Virtual is where a session starts and where it returns after idle
//! teardown. A call promotes it to compute when [`decide`] says so, when the
//! agent asks for compute, or when it starts a process — always before
//! anything of the call runs. A virtual run that fails is a failed run;
//! nothing is run again in the sandbox after the fact. Promotion is sticky
//! until idle teardown.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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

use crate::compute::{Sandbox, SandboxConfig, SandboxFiles};
use crate::decide::{Decision, decide};
use crate::files::VirtualFiles;
use crate::layer::Layer;
use crate::mount::{ErrorLog, MountBackend, MountError, MountTable};
use crate::quota::{Quota, Usage};
use crate::shell::{self, VirtualCommand};
use crate::vfs::ScratchFs;

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
    /// Sessions live under `<root>/sessions/<session>`; runsc keeps its state
    /// under `<root>/runsc`.
    pub root: PathBuf,
    /// The base image, exported as a directory. Read-only to scratch.
    pub rootfs: PathBuf,
    /// Served read-only at `/opt/toolstore` in both modes.
    pub toolstore: Option<PathBuf>,
    /// Without runsc there is no compute: a call that needs it fails, and
    /// says why.
    pub runsc: Option<PathBuf>,
    /// The static `scratch-helper` binary, for files and `/mnt` in compute.
    pub helper: Option<PathBuf>,
    /// Bytes one user's sessions may hold in total.
    pub quota: Option<u64>,
    /// How long a session sits unused before its sandbox is stopped.
    pub idle_timeout: Duration,
    /// The environment every command starts with, in both modes.
    pub env: Vec<(String, String)>,
    /// `$HOME` in the image.
    pub home: String,
    /// The shell `exec` runs through in compute; the virtual shell is bash.
    pub shell: String,
    pub network: bool,
    pub rootless: bool,
    pub runsc_flags: Vec<String>,
}

/// Unused this long, a sandbox is stopped and its layer archived.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

impl ScratchConfig {
    /// Defaults matching the Debian base image.
    pub fn new(root: impl Into<PathBuf>, rootfs: impl Into<PathBuf>) -> Self {
        ScratchConfig {
            root: root.into(),
            rootfs: rootfs.into(),
            toolstore: None,
            runsc: None,
            helper: None,
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
            shell: "/bin/bash".to_owned(),
            network: false,
            rootless: false,
            runsc_flags: Vec::new(),
        }
    }

    fn sandbox(&self) -> Option<SandboxConfig> {
        Some(SandboxConfig {
            runsc: self.runsc.clone()?,
            helper: self.helper.clone(),
            state: self.root.join("runsc"),
            rootfs: self.rootfs.clone(),
            toolstore: self.toolstore.clone(),
            env: self.env.clone(),
            shell: self.shell.clone(),
            network: self.network,
            rootless: self.rootless,
            flags: self.runsc_flags.clone(),
        })
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
    Compute(Arc<Sandbox>),
}

/// What the dispatching spawn and files share with the session.
struct Inner {
    id: String,
    user: String,
    dir: PathBuf,
    config: Arc<ScratchConfig>,
    sandbox_config: Option<Arc<SandboxConfig>>,
    quota: Arc<Quota>,
    usage: Usage,
    layer: Arc<Layer>,
    fs: Arc<ScratchFs>,
    mounts: Arc<MountTable>,
    errors: Arc<ErrorLog>,
    /// Virtual work holds a read guard for as long as it touches the layer;
    /// switching modes takes the write guard, so a promotion never exports a
    /// layer something is still writing.
    state: Arc<RwLock<State>>,
}

impl Inner {
    fn layer_tar(&self) -> PathBuf {
        self.dir.join("layer.tar")
    }

    /// Switches to compute, starting the sandbox over the current layer.
    /// Refuses, having started nothing, when there is no runsc or the user is
    /// out of quota.
    async fn promote(&self, why: &str) -> Result<Arc<Sandbox>, Fault> {
        let mut state = self.state.write().await;
        if let State::Compute(sandbox) = &*state {
            return Ok(Arc::clone(sandbox));
        }
        self.quota
            .check(&self.user)
            .map_err(|exceeded| refused(format!("this needs compute ({why}), but {exceeded}")))?;
        let Some(config) = &self.sandbox_config else {
            return Err(refused(format!(
                "this needs compute ({why}), and this Faber server has no sandbox runtime"
            )));
        };

        let tar = self.layer_tar();
        let layer = Arc::clone(&self.layer);
        let staged = tar.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let file = std::fs::File::create(&staged)?;
            layer
                .export_tar(std::io::BufWriter::new(file))?
                .into_inner()?
                .sync_all()
        })
        .await
        .map_err(|error| Fault::Unreachable(error.to_string()))?
        .map_err(|error| {
            Fault::Unreachable(format!("scratch could not archive its layer: {error}"))
        })?;

        // The sandbox may grow the session to whatever the user has left,
        // reserved so that another session promoted meanwhile cannot be
        // granted the same bytes. Demoting sets the real size again.
        let before = self.usage.bytes();
        let size = self.quota.reserve(&self.usage);
        let sandbox =
            match Sandbox::start(Arc::clone(config), &self.id, &self.dir, Some(&tar), size).await {
                Ok(sandbox) => Arc::new(sandbox),
                Err(fault) => {
                    self.usage.set(before);
                    return Err(fault);
                }
            };
        if config.helper.is_some()
            && let Err(fault) = sandbox
                .serve_mounts(Arc::clone(&self.mounts), Arc::clone(&self.errors))
                .await
        {
            tracing::warn!(session = %self.id, %fault, "scratch compute runs without /mnt");
        }
        tracing::info!(session = %self.id, reason = %why, "scratch promoted to compute");
        *state = State::Compute(Arc::clone(&sandbox));
        Ok(sandbox)
    }

    /// Archives the sandbox's layer and stops it. Returns to virtual mode
    /// over the archived layer; on failure the sandbox keeps running, so
    /// nothing in it is lost.
    async fn demote(&self) -> Result<(), Fault> {
        let mut state = self.state.write().await;
        let State::Compute(sandbox) = &*state else {
            return Ok(());
        };
        let tar = self.dir.join("upper.tar");
        sandbox.export_upper(&tar).await?;
        let layer = Arc::clone(&self.layer);
        let staged = tar.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            layer.import_tar(std::io::BufReader::new(std::fs::File::open(&staged)?))
        })
        .await
        .map_err(|error| Fault::Unreachable(error.to_string()))?
        .map_err(|error| {
            Fault::Unreachable(format!("scratch could not restore its layer: {error}"))
        })?;
        let _ = tokio::fs::remove_file(&tar).await;
        let _ = tokio::fs::remove_file(self.layer_tar()).await;
        self.usage.set(self.layer.bytes());
        sandbox.stop().await;
        *state = State::Virtual;
        tracing::info!(session = %self.id, "scratch stopped its idle sandbox");
        Ok(())
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
        let sandbox = match &*state {
            State::Compute(sandbox) => Arc::clone(sandbox),
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
        sandbox.spawn(run).await
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
            let mut env = inner.config.env.clone();
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
        compute: impl AsyncFnOnce(&SandboxFiles) -> Result<T, Fault>,
    ) -> Result<T, Fault> {
        let state = self.inner.state.read().await;
        match &*state {
            State::Virtual => {
                virtual_(&VirtualFiles {
                    fs: Arc::clone(&self.inner.fs),
                })
                .await
            }
            State::Compute(sandbox) => {
                compute(&SandboxFiles {
                    sandbox: Arc::clone(sandbox),
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

    /// The mode the session is in now. Read after a call, it is the mode
    /// that call ran in: promotion is sticky, and only idle teardown — which
    /// waits for calls in flight — returns to virtual.
    pub async fn mode(&self) -> Mode {
        Inner::mode(&*self.inner.state.read().await)
    }

    fn call(&self) -> Busy<'_> {
        self.busy.fetch_add(1, Ordering::SeqCst);
        *self.last_call.lock().expect("clock poisoned") = Instant::now();
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
    /// sandbox at once.
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

    /// Bytes this session's layer holds, as last counted.
    pub fn bytes(&self) -> u64 {
        self.inner.usage.bytes()
    }

    /// Stops the sandbox if the session has been idle for `idle` with no
    /// call in flight and no managed process (one from `process start`)
    /// running. Anything else still running — what an `exec` left behind —
    /// is stopped with it. Returns whether it stopped the sandbox.
    pub async fn teardown_idle(&self, idle: Duration) -> bool {
        if self.busy.load(Ordering::SeqCst) > 0
            || self.last_call.lock().expect("clock poisoned").elapsed() < idle
            || self.mode().await == Mode::Virtual
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
        self.teardown().await.is_ok()
    }

    /// Archives the layer and stops the sandbox now. Background processes
    /// still running are stopped with it; a later call on one of them says
    /// so.
    pub async fn teardown(&self) -> Result<(), Fault> {
        if self.mode().await == Mode::Virtual {
            return Ok(());
        }
        let running: Vec<ProcId> = self
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
            .unwrap_or_default();
        self.inner.demote().await?;
        self.stopped
            .lock()
            .expect("stopped poisoned")
            .extend(running);
        Ok(())
    }

    /// Refuses a call on a process idle teardown stopped.
    fn live(&self, id: ProcId) -> Result<(), Fault> {
        if self.stopped.lock().expect("stopped poisoned").contains(&id) {
            return Err(Fault::Denied(Denial::Stopped(id)));
        }
        Ok(())
    }

    /// Stops everything without archiving, for deletion.
    async fn kill(&self) {
        let mut state = self.inner.state.write().await;
        if let State::Compute(sandbox) = &*state {
            sandbox.stop().await;
        }
        *state = State::Virtual;
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
    sandbox_config: Option<Arc<SandboxConfig>>,
    quota: Arc<Quota>,
    sessions: tokio::sync::Mutex<HashMap<String, Arc<Session>>>,
}

/// A session id becomes a directory name and a sandbox id, so it is held to
/// what both accept. Faber's are UUIDs.
fn valid_session(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl ScratchManager {
    /// Opens the scratch root, counting what earlier sessions left on disk
    /// against their users' quota.
    pub fn new(config: ScratchConfig) -> Result<Self, ScratchError> {
        if !config.rootfs.is_dir() {
            return Err(ScratchError::Unavailable(format!(
                "the base image `{}` is not a directory",
                config.rootfs.display()
            )));
        }
        std::fs::create_dir_all(config.root.join("sessions"))
            .map_err(|error| ScratchError::Storage(error.to_string()))?;
        let quota = Arc::new(Quota::new(config.quota));
        if let Ok(entries) = std::fs::read_dir(config.root.join("sessions")) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                let Ok(user) = std::fs::read_to_string(entry.path().join("owner")) else {
                    continue;
                };
                if let Ok(layer) = Layer::open(entry.path().join("layer")) {
                    let _ = quota.track(user.trim(), &id, layer.bytes());
                }
            }
        }
        let sandbox_config = config.sandbox().map(Arc::new);
        Ok(ScratchManager {
            config: Arc::new(config),
            sandbox_config,
            quota,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
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

    /// Archives and stops sandboxes an earlier Faber process left running.
    /// Call once at startup, before any session is opened.
    pub async fn recover(&self) {
        let Some(config) = &self.sandbox_config else {
            return;
        };
        for id in crate::compute::running(config).await {
            let dir = self.dir(&id);
            let sandbox = crate::compute::orphan(Arc::clone(config), &id, &dir);
            if valid_session(&id) && dir.join("layer").is_dir() {
                let tar = dir.join("upper.tar");
                let restored = match sandbox.export_upper(&tar).await {
                    Ok(()) => Layer::open(dir.join("layer")).and_then(|layer| {
                        layer.import_tar(std::io::BufReader::new(std::fs::File::open(&tar)?))
                    }),
                    Err(fault) => Err(std::io::Error::other(fault.to_string())),
                };
                if let Err(error) = restored {
                    tracing::warn!(session = %id, %error, "a running sandbox's layer was lost");
                }
                let _ = std::fs::remove_file(&tar);
            }
            sandbox.stop().await;
        }
    }

    /// The session's scratch, opened on first use. `blobs` is where its
    /// results are stored, fixed when the session is first opened.
    pub async fn session(
        &self,
        id: &str,
        user: &str,
        blobs: Arc<dyn Blobs>,
    ) -> Result<Arc<Session>, ScratchError> {
        if !valid_session(id) {
            return Err(ScratchError::InvalidSession(id.to_owned()));
        }
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(id) {
            return Ok(Arc::clone(session));
        }
        let dir = self.dir(id);
        let storage = |error: std::io::Error| ScratchError::Storage(error.to_string());
        std::fs::create_dir_all(&dir).map_err(storage)?;
        // The first owner stays the owner: the quota a session counts against
        // never moves.
        let owner = match std::fs::read_to_string(dir.join("owner")) {
            Ok(owner) => owner.trim().to_owned(),
            Err(_) => {
                std::fs::write(dir.join("owner"), user).map_err(storage)?;
                user.to_owned()
            }
        };
        let layer = Arc::new(Layer::open(dir.join("layer")).map_err(storage)?);
        let usage = self.quota.track(&owner, id, layer.bytes());
        let mounts = Arc::new(MountTable::default());
        let errors = Arc::new(ErrorLog::default());
        let fs = Arc::new(ScratchFs::new(
            self.config.rootfs.clone(),
            self.config.toolstore.clone(),
            Arc::clone(&layer),
            Arc::clone(&mounts),
            Arc::clone(&errors),
            usage.clone(),
        ));
        let inner = Arc::new(Inner {
            id: id.to_owned(),
            user: owner,
            dir,
            config: Arc::clone(&self.config),
            sandbox_config: self.sandbox_config.clone(),
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

    /// Stops every sandbox idle past the configured timeout.
    pub async fn teardown_idle(&self) {
        let sessions: Vec<Arc<Session>> = self.sessions.lock().await.values().cloned().collect();
        for session in sessions {
            session.teardown_idle(self.config.idle_timeout).await;
        }
    }

    /// Deletes a session's scratch with everything in it.
    pub async fn delete_session(&self, id: &str) {
        if !valid_session(id) {
            return;
        }
        let removed = self.sessions.lock().await.remove(id);
        let owner = match &removed {
            Some(session) => {
                session.kill().await;
                Some(session.user().to_owned())
            }
            None => std::fs::read_to_string(self.dir(id).join("owner"))
                .ok()
                .map(|owner| owner.trim().to_owned()),
        };
        if removed.is_none()
            && let Some(config) = &self.sandbox_config
        {
            crate::compute::orphan(Arc::clone(config), id, &self.dir(id))
                .stop()
                .await;
        }
        if let Some(owner) = owner {
            self.quota.forget(&owner, id);
        }
        let _ = tokio::fs::remove_dir_all(self.dir(id)).await;
    }
}

/// For tests and tools: where a session's layer is.
pub fn layer_dir(root: &Path, session: &str) -> PathBuf {
    root.join("sessions").join(session).join("layer")
}
