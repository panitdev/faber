//! Compute mode: a gVisor sandbox driven through `runsc`.
//!
//! One sandbox per session, started on promotion and stopped at idle
//! teardown. Its root filesystem is the base image under an overlay whose
//! upper layer starts as the session's layer, handed over as a tar through
//! the `dev.gvisor.tar.rootfs.upper` annotation (runsc only honours it with
//! `--allow-rootfs-tar-annotation`), and comes back the same way through
//! `runsc tar rootfs-upper`. The base directory itself is never written: the
//! overlay's upper lives in a filestore under the session's directory.
//!
//! Every command, background process and file operation is a `runsc exec`
//! into the running sandbox. File operations run `scratch-helper`, which the
//! bundle bind-mounts read-only at [`HELPER`], so they never depend on what
//! the image carries. (`runsc exec --exec-fd` would avoid the mount, but a
//! process started that way sees `/proc/self/exe` as `host:[N]`, and a
//! statically linked glibc program aborts at startup on exactly that.)
//!
//! The `runsc` client is started with an empty environment: nothing of the
//! Faber process's own environment reaches it or the sandbox. A sandboxed
//! process's environment is the bundle's plus the call's, exactly the list
//! the virtual shell runs with.

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use environment::files::{Confined, DirEntry, Files};
use environment::spawn::{Proc, Run, Sink, Source};
use environment::{Denial, EntryKind, Fault, Outcome, RootedPath, Signal, Stat};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::fuse::MntFs;
use crate::mount::{ErrorLog, MountTable};

/// The annotation runsc reads the rootfs upper layer from.
pub const UPPER_ANNOTATION: &str = "dev.gvisor.tar.rootfs.upper";

/// Where `scratch-helper` appears in the sandbox: under `/dev`, which the
/// sandbox mounts for itself and virtual mode never shows, so the image's own
/// tree is the same in both modes.
pub const HELPER: &str = "/dev/.faber/scratch-helper";

/// How long the relay may take to mount `/mnt` before compute goes on
/// without it.
const RELAY_READY: Duration = Duration::from_secs(10);

/// What every sandbox of a manager shares.
#[derive(Clone, Debug)]
pub struct SandboxConfig {
    pub runsc: PathBuf,
    /// The static `scratch-helper`. Without it compute still runs commands,
    /// but `read` and `patch` cannot reach the sandbox's files and `/mnt` is
    /// not served.
    pub helper: Option<PathBuf>,
    /// Where runsc keeps container state (`--root`).
    pub state: PathBuf,
    pub rootfs: PathBuf,
    pub toolstore: Option<PathBuf>,
    /// The environment every process starts with.
    pub env: Vec<(String, String)>,
    /// The shell `exec` runs through, an absolute path in the image.
    pub shell: String,
    /// Give the sandbox gVisor's own network stack. Off, it has none.
    pub network: bool,
    /// `--rootless`, for a Faber process that is not root.
    pub rootless: bool,
    /// Further global runsc flags, such as `--platform=kvm`.
    pub flags: Vec<String>,
}

/// A running sandbox.
pub struct Sandbox {
    id: String,
    config: Arc<SandboxConfig>,
    /// `--overlay2` for this sandbox, which carries its size cap.
    overlay: String,
    bundle: PathBuf,
    relay: Mutex<Option<Relay>>,
}

/// The FUSE server and the relay process feeding it.
struct Relay {
    _child: Child,
    _session: fuser::BackgroundSession,
}

/// A failure to run `runsc` itself, as opposed to a command failing in it.
fn unreachable(what: &str, error: impl std::fmt::Display) -> Fault {
    Fault::Unreachable(format!("scratch sandbox: {what}: {error}"))
}

impl Sandbox {
    /// Creates and starts a sandbox for session `id`.
    ///
    /// `upper` is the session's layer as a tar in gVisor's format, if it has
    /// anything in it. `size` caps the overlay's upper layer — what the
    /// session may hold in total under its user's quota.
    pub async fn start(
        config: Arc<SandboxConfig>,
        id: &str,
        dir: &Path,
        upper: Option<&Path>,
        size: Option<u64>,
    ) -> Result<Sandbox, Fault> {
        let bundle = dir.join("bundle");
        let filestore = bundle.join("filestore");
        // A sandbox from an earlier start of this process may be left over.
        let _ = tokio::fs::remove_dir_all(&bundle).await;
        tokio::fs::create_dir_all(&filestore)
            .await
            .map_err(|error| unreachable("creating its bundle", error))?;
        tokio::fs::create_dir_all(&config.state)
            .await
            .map_err(|error| unreachable("creating runsc's state directory", error))?;

        let mut overlay = format!("root:dir={}", filestore.display());
        if let Some(size) = size {
            // gVisor reads sizes in bytes; a zero cap would mean "default".
            overlay.push_str(&format!(",size={}", size.max(1)));
        }

        let spec = bundle_spec(&config, upper);
        tokio::fs::write(
            bundle.join("config.json"),
            serde_json::to_vec_pretty(&spec).expect("spec serializes"),
        )
        .await
        .map_err(|error| unreachable("writing its bundle", error))?;

        let sandbox = Sandbox {
            id: id.to_owned(),
            config,
            overlay,
            bundle,
            relay: Mutex::new(None),
        };
        // Whatever is left of an earlier sandbox under this id is gone.
        sandbox.destroy().await;
        // The detached sandbox inherits whatever the client's output is
        // connected to and keeps it open for its whole life, so the output
        // goes to a file: a pipe would never reach end of file.
        let log = sandbox.bundle.join("start.log");
        let file = std::fs::File::create(&log)
            .map_err(|error| unreachable("creating its start log", error))?;
        let status = sandbox
            .runsc()
            .args(["run", "-detach", "-bundle"])
            .arg(&sandbox.bundle)
            .arg(&sandbox.id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(file)
            .status()
            .await
            .map_err(|error| unreachable("running runsc", error))?;
        if !status.success() {
            let reason = std::fs::read_to_string(&log).unwrap_or_default();
            return Err(unreachable("starting", reason.trim()));
        }
        Ok(sandbox)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// `runsc` with the global flags, an empty environment, and the bundle as
    /// its working directory.
    fn runsc(&self) -> Command {
        let mut command = Command::new(&self.config.runsc);
        command
            .env_clear()
            .current_dir(&self.bundle)
            .kill_on_drop(true);
        command.arg(format!("--root={}", self.config.state.display()));
        command.arg(if self.config.network {
            "--network=sandbox"
        } else {
            "--network=none"
        });
        if self.config.rootless {
            command.args(["--rootless", "--ignore-cgroups"]);
        }
        command.arg(format!("--overlay2={}", self.overlay));
        command.arg("--allow-rootfs-tar-annotation");
        command.args(&self.config.flags);
        command
    }

    /// `runsc exec` of `argv`, with the caller's cwd and environment.
    fn exec_command(&self, run: &Run, pid_file: &Path) -> Command {
        let mut command = self.runsc();
        command.arg("exec").arg(format!("-cwd={}", run.cwd));
        for (key, value) in &run.env {
            command.arg(format!("-env={key}={value}"));
        }
        command
            .arg(format!("-internal-pid-file={}", pid_file.display()))
            .arg(&self.id)
            .args(&run.argv);
        command
    }

    /// Starts one process in the sandbox.
    pub async fn spawn(&self, run: Run) -> Result<Box<dyn Proc>, Fault> {
        if run.pty {
            return Err(Fault::Denied(Denial::MissingCapability(
                environment::Capability::Pty,
            )));
        }
        let pid_file = self.bundle.join(format!("pid-{}", next_serial()));
        let mut child = self
            .exec_command(&run, &pid_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| unreachable("running runsc exec", error))?;
        Ok(Box::new(SandboxProc {
            stdin: child.stdin.take().map(|pipe| Box::new(pipe) as Sink),
            stdout: child.stdout.take().map(|pipe| Box::new(pipe) as Source),
            stderr: child.stderr.take().map(|pipe| Box::new(pipe) as Source),
            child,
            runsc: self.kill_command(),
            pid_file,
            signalled: None,
            outcome: None,
        }))
    }

    /// What signalling a process needs, detached from the sandbox's lifetime.
    fn kill_command(&self) -> KillCommand {
        KillCommand {
            runsc: self.config.runsc.clone(),
            state: self.config.state.clone(),
            rootless: self.config.rootless,
            id: self.id.clone(),
        }
    }

    /// Runs `scratch-helper` in the sandbox. Returns its stdout, or the errno
    /// it reported.
    pub async fn helper(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<Vec<u8>, HelperError> {
        if self.config.helper.is_none() {
            return Err(HelperError::Fault(Fault::Unreachable(
                "scratch cannot reach files in its sandbox: no scratch-helper is configured"
                    .to_owned(),
            )));
        }
        let mut command = self.runsc();
        command
            .args(["exec", "-cwd=/"])
            .arg(&self.id)
            .arg(HELPER)
            .args(args);
        command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| HelperError::Fault(unreachable("running runsc exec", error)))?;
        if let (Some(body), Some(mut pipe)) = (stdin, child.stdin.take()) {
            let body = body.to_vec();
            tokio::spawn(async move {
                let _ = pipe.write_all(&body).await;
                let _ = pipe.shutdown().await;
            });
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|error| HelperError::Fault(unreachable("running runsc exec", error)))?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        // The helper's own failures are `errno N message`; anything else is
        // runsc failing to run it at all.
        match stderr
            .lines()
            .find_map(|line| line.strip_prefix("errno "))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|code| code.parse::<i32>().ok())
        {
            Some(code) => Err(HelperError::Errno(code, stderr.trim().to_owned())),
            None => Err(HelperError::Fault(unreachable(
                "scratch-helper",
                stderr.trim(),
            ))),
        }
    }

    /// Mounts FUSE at `/mnt` in the sandbox, served by a [`MntFs`] over the
    /// session's mount table. Waits until the mount exists.
    pub async fn serve_mounts(
        &self,
        mounts: Arc<MountTable>,
        errors: Arc<ErrorLog>,
    ) -> Result<(), Fault> {
        if self.config.helper.is_none() {
            return Err(Fault::Unreachable(
                "no scratch-helper is configured, so /mnt is not served".to_owned(),
            ));
        }
        let (ours, theirs) =
            seqpacket_pair().map_err(|error| unreachable("creating the /mnt socket", error))?;

        let mut command = self.runsc();
        command
            .args(["exec", "-cwd=/", "-cap=CAP_SYS_ADMIN", "-pass-fd=3:3"])
            .arg(&self.id)
            .args([HELPER, "relay", crate::mount::MNT]);
        place_fds(&mut command, vec![(theirs.as_raw_fd(), 3)]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| unreachable("starting the /mnt relay", error))?;
        drop(theirs);

        // The FUSE handshake blocks until the kernel side sends INIT, which
        // happens when the relay mounts; it runs on a thread of its own.
        let runtime = tokio::runtime::Handle::current();
        let server = MntFs::new(mounts, errors, runtime);
        let serving = tokio::task::spawn_blocking(move || server.serve(ours));

        tracing::debug!(sandbox = %self.id, "waiting for the /mnt relay");
        let stdout = child.stdout.take().expect("piped");
        let mut lines = BufReader::new(stdout).lines();
        let ready = tokio::time::timeout(RELAY_READY, lines.next_line()).await;
        match ready {
            Ok(Ok(Some(line))) if line == "ready" => {}
            other => {
                let _ = child.start_kill();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        pipe.read_to_string(&mut stderr),
                    )
                    .await;
                }
                return Err(unreachable(
                    "mounting /mnt",
                    format!("{other:?} {}", stderr.trim()),
                ));
            }
        }
        let session = tokio::time::timeout(RELAY_READY, serving)
            .await
            .map_err(|_| unreachable("serving /mnt", "the FUSE handshake did not finish"))?
            .map_err(|error| unreachable("serving /mnt", error))?
            .map_err(|error| unreachable("serving /mnt", error))?;
        tracing::debug!(sandbox = %self.id, "/mnt is served");
        *self.relay.lock().expect("relay poisoned") = Some(Relay {
            _child: child,
            _session: session,
        });
        Ok(())
    }

    /// Writes the overlay's upper layer to `to` as a tar.
    pub async fn export_upper(&self, to: &Path) -> Result<(), Fault> {
        let output = self
            .runsc()
            .args(["tar", "rootfs-upper"])
            .arg(format!("-file={}", to.display()))
            .arg(&self.id)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| unreachable("running runsc tar", error))?;
        if !output.status.success() {
            return Err(unreachable(
                "exporting its layer",
                String::from_utf8_lossy(&output.stderr).trim(),
            ));
        }
        Ok(())
    }

    /// Stops the sandbox and everything in it, and removes its bundle.
    pub async fn stop(&self) {
        self.relay.lock().expect("relay poisoned").take();
        self.destroy().await;
        let _ = tokio::fs::remove_dir_all(&self.bundle).await;
    }

    async fn destroy(&self) {
        let _ = self
            .runsc()
            .args(["kill", &self.id, "KILL"])
            .stdin(Stdio::null())
            .output()
            .await;
        let _ = self
            .runsc()
            .args(["delete", "-force", &self.id])
            .stdin(Stdio::null())
            .output()
            .await;
    }
}

/// Ids of sandboxes runsc still knows under `state`, for recovering after the
/// Faber process restarted while some were running.
pub async fn running(config: &SandboxConfig) -> Vec<String> {
    let mut command = Command::new(&config.runsc);
    command
        .env_clear()
        .arg(format!("--root={}", config.state.display()));
    if config.rootless {
        command.args(["--rootless", "--ignore-cgroups"]);
    }
    let Ok(output) = command
        .args(["list", "-quiet"])
        .stdin(Stdio::null())
        .output()
        .await
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A sandbox left by an earlier Faber process, reattached only to export its
/// layer and stop it.
pub fn orphan(config: Arc<SandboxConfig>, id: &str, dir: &Path) -> Sandbox {
    let bundle = dir.join("bundle");
    let overlay = format!("root:dir={}", bundle.join("filestore").display());
    Sandbox {
        id: id.to_owned(),
        config,
        overlay,
        bundle,
        relay: Mutex::new(None),
    }
}

fn next_serial() -> u64 {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The OCI bundle: the base image as root, writable only through the overlay,
/// the toolstore read-only, no network unless configured, and an init that
/// does nothing but keep the sandbox up.
fn bundle_spec(config: &SandboxConfig, upper: Option<&Path>) -> serde_json::Value {
    let env: Vec<String> = config
        .env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    // What a root shell doing file and build work needs. No `CAP_MKNOD`: a
    // 0/0 character device is how a layer records a deletion, and nothing
    // in scratch needs another device. No raw sockets, no chroot, no
    // `setpcap`; the network caps only when there is a network.
    let mut caps = vec![
        "CAP_CHOWN",
        "CAP_DAC_OVERRIDE",
        "CAP_FSETID",
        "CAP_FOWNER",
        "CAP_SETGID",
        "CAP_SETUID",
        "CAP_KILL",
    ];
    if config.network {
        caps.push("CAP_NET_BIND_SERVICE");
    }
    let mut mounts = vec![
        json!({"destination": "/proc", "type": "proc", "source": "proc"}),
        json!({"destination": "/dev", "type": "tmpfs", "source": "tmpfs"}),
        json!({"destination": "/sys", "type": "sysfs", "source": "sysfs",
               "options": ["nosuid", "noexec", "nodev", "ro"]}),
    ];
    if let Some(helper) = &config.helper {
        mounts.push(json!({
            "destination": HELPER,
            "type": "bind",
            "source": helper,
            "options": ["bind", "ro", "nosuid", "nodev"],
        }));
    }
    if let Some(toolstore) = &config.toolstore {
        mounts.push(json!({
            "destination": crate::vfs::TOOLSTORE,
            "type": "bind",
            "source": toolstore,
            "options": ["rbind", "ro"],
        }));
    }
    let mut annotations = serde_json::Map::new();
    if let Some(upper) = upper {
        annotations.insert(UPPER_ANNOTATION.to_owned(), json!(upper));
    }
    json!({
        "ociVersion": "1.0.0",
        "process": {
            "terminal": false,
            "user": {"uid": 0, "gid": 0},
            // Only the shell is assumed of the image; `sleep` is in coreutils
            // and busybox alike.
            "args": [config.shell, "-c", "while :; do sleep 86400; done"],
            "env": env,
            "cwd": "/",
            "capabilities": {
                "bounding": caps, "effective": caps, "permitted": caps,
            },
            "rlimits": [{"type": "RLIMIT_NOFILE", "hard": 65536, "soft": 65536}],
            "noNewPrivileges": true,
        },
        "root": {"path": config.rootfs, "readonly": false},
        "hostname": "scratch",
        "mounts": mounts,
        "annotations": annotations,
        "linux": {
            "namespaces": [
                {"type": "pid"}, {"type": "network"}, {"type": "ipc"},
                {"type": "uts"}, {"type": "mount"},
            ],
        },
    })
}

/// Makes `fds` appear at the given numbers in the child, and nothing else
/// leak: every source is first moved above the targets, so a source that
/// already sits on a target number is not clobbered.
fn place_fds(command: &mut Command, fds: Vec<(RawFd, RawFd)>) {
    // Safety: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(move || {
            let mut moved = Vec::with_capacity(fds.len());
            for (source, target) in &fds {
                let high = libc::fcntl(*source, libc::F_DUPFD_CLOEXEC, 64);
                if high < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                moved.push((high, *target));
            }
            for (high, target) in moved {
                if libc::dup2(high, target) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

fn seqpacket_pair() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // Safety: socketpair writes two descriptors into `fds` on success.
    let made = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    };
    if made != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Safety: freshly created and owned by nothing else.
    use std::os::fd::FromRawFd;
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// What a helper call returned instead of output.
#[derive(Debug)]
pub enum HelperError {
    /// The helper ran and the system call failed with this errno.
    Errno(i32, String),
    /// The helper could not be run.
    Fault(Fault),
}

/// Signals a process by its pid inside the sandbox.
#[derive(Clone)]
struct KillCommand {
    runsc: PathBuf,
    state: PathBuf,
    rootless: bool,
    id: String,
}

impl KillCommand {
    async fn send(&self, flag: &str, pid: u32, signal: Signal) -> bool {
        let mut command = Command::new(&self.runsc);
        command
            .env_clear()
            .arg(format!("--root={}", self.state.display()));
        if self.rootless {
            command.args(["--rootless", "--ignore-cgroups"]);
        }
        command
            .arg("kill")
            .arg(format!("{flag}={pid}"))
            .arg(&self.id)
            .arg(signal.number().to_string())
            .stdin(Stdio::null());
        command
            .output()
            .await
            .is_ok_and(|output| output.status.success())
    }
}

/// A process started with `runsc exec`.
struct SandboxProc {
    stdin: Option<Sink>,
    stdout: Option<Source>,
    stderr: Option<Source>,
    child: Child,
    runsc: KillCommand,
    pid_file: PathBuf,
    /// The signal last sent, to read the client's `128 + n` exit back as the
    /// signal it is.
    signalled: Option<Signal>,
    outcome: Option<Outcome>,
}

impl SandboxProc {
    fn outcome_of(&mut self, status: std::process::ExitStatus) -> Outcome {
        let _ = std::fs::remove_file(&self.pid_file);
        let outcome = match (status.code(), self.signalled) {
            (Some(code), Some(signal)) if code == 128 + signal.number() => {
                Outcome::Signaled { signal }
            }
            (Some(code), _) => Outcome::Completed { code },
            (None, _) => {
                use std::os::unix::process::ExitStatusExt;
                match status.signal().and_then(Signal::from_number) {
                    Some(signal) => Outcome::Signaled { signal },
                    None => Outcome::Completed { code: -1 },
                }
            }
        };
        self.outcome = Some(outcome);
        outcome
    }

    fn pid(&self) -> Option<u32> {
        std::fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
    }
}

#[async_trait]
impl Proc for SandboxProc {
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
        if let Some(outcome) = self.outcome {
            return Ok(outcome);
        }
        let status = self
            .child
            .wait()
            .await
            .map_err(|error| Fault::Unreachable(error.to_string()))?;
        Ok(self.outcome_of(status))
    }

    async fn try_wait(&mut self) -> Result<Option<Outcome>, Fault> {
        if let Some(outcome) = self.outcome {
            return Ok(Some(outcome));
        }
        match self.child.try_wait() {
            Ok(Some(status)) => Ok(Some(self.outcome_of(status))),
            Ok(None) => Ok(None),
            Err(error) => Err(Fault::Unreachable(error.to_string())),
        }
    }

    /// Signals the process inside the sandbox — its process group first, so
    /// a shell's children go with it, then the process alone. Signalling the
    /// `runsc exec` client instead would forward a catchable signal but could
    /// never forward `KILL`.
    async fn signal(&mut self, signal: Signal) -> Result<(), Fault> {
        if self.outcome.is_some() {
            return Err(Fault::Unreachable("the process handle is spent".to_owned()));
        }
        self.signalled = Some(signal);
        if let Some(pid) = self.pid()
            && (self.runsc.send("-pgid", pid, signal).await
                || self.runsc.send("-pid", pid, signal).await)
        {
            return Ok(());
        }
        // No pid yet: the client forwards what it can.
        let Some(client) = self.child.id() else {
            return Err(Fault::Unreachable("the process has no pid".to_owned()));
        };
        // Safety: `kill(2)` on a child this handle owns and has not reaped.
        let sent = unsafe { libc::kill(client as libc::pid_t, signal.number()) };
        if sent != 0 {
            return Err(Fault::Unreachable(
                std::io::Error::last_os_error().to_string(),
            ));
        }
        Ok(())
    }
}

/// `read` and `patch` while scratch is in compute: the session's files are in
/// the sandbox's overlay, reached through `scratch-helper`.
pub struct SandboxFiles {
    pub sandbox: Arc<Sandbox>,
}

fn helper_fault(path: &RootedPath, error: HelperError) -> Fault {
    match error {
        HelperError::Errno(code, _) if code == libc::ENOENT => Fault::Denied(Denial::NotFound {
            path: path.to_string(),
        }),
        HelperError::Errno(_, message) => Fault::Unreachable(message),
        HelperError::Fault(fault) => fault,
    }
}

/// `KIND SIZE MTIME REALPATH`, as the helper prints it.
fn parse_stat(output: &[u8]) -> Option<(EntryKind, u64, u64, String)> {
    let text = std::str::from_utf8(output).ok()?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let mut parts = text.splitn(4, ' ');
    let kind = match parts.next()? {
        "dir" => EntryKind::Dir,
        "file" => EntryKind::File,
        "symlink" => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    let size = parts.next()?.parse().ok()?;
    let mtime = parts.next()?.parse().ok()?;
    Some((kind, size, mtime, parts.next()?.to_owned()))
}

fn stat_of(path: &RootedPath, output: &[u8]) -> Result<Stat, Fault> {
    let (_, size, mtime, _) = parse_stat(output)
        .ok_or_else(|| unreachable("scratch-helper", "an unreadable stat line"))?;
    Ok(Stat {
        path: path.clone(),
        size,
        modified: Some(std::time::UNIX_EPOCH + Duration::from_secs(mtime)),
    })
}

#[async_trait]
impl Files for SandboxFiles {
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault> {
        // The sandbox is the boundary; all that is left is to say what is
        // there.
        match self.sandbox.helper(&["stat", path.as_str()], None).await {
            Ok(output) => {
                let (kind, _, _, real) = parse_stat(&output)
                    .ok_or_else(|| unreachable("scratch-helper", "an unreadable stat line"))?;
                Ok(Confined {
                    path: real,
                    kind: Some(kind),
                })
            }
            Err(HelperError::Errno(..)) => Ok(Confined {
                path: path.as_str().to_owned(),
                kind: None,
            }),
            Err(HelperError::Fault(fault)) => Err(fault),
        }
    }

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        self.sandbox
            .helper(&["cat", path.as_str()], None)
            .await
            .map_err(|error| helper_fault(path, error))
    }

    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        let output = self
            .sandbox
            .helper(&["put", path.as_str()], Some(body))
            .await
            .map_err(|error| helper_fault(path, error))?;
        stat_of(path, &output)
    }

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault> {
        self.sandbox
            .helper(&["rm", path.as_str()], None)
            .await
            .map(|_| ())
            .map_err(|error| helper_fault(path, error))
    }

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault> {
        let output = self
            .sandbox
            .helper(&["mv", from.as_str(), to.as_str()], None)
            .await
            .map_err(|error| helper_fault(from, error))?;
        stat_of(to, &output)
    }

    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault> {
        let output = self
            .sandbox
            .helper(&["ls", dir.as_str()], None)
            .await
            .map_err(|error| helper_fault(dir, error))?;
        let fields: Vec<&[u8]> = output.split(|byte| *byte == 0).collect();
        let mut entries = Vec::new();
        for record in fields.chunks(3) {
            let [kind, size, name] = record else { break };
            let kind = match *kind {
                b"dir" => EntryKind::Dir,
                b"file" => EntryKind::File,
                b"symlink" => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
            entries.push(DirEntry {
                name: String::from_utf8_lossy(name).into_owned(),
                kind,
                size: std::str::from_utf8(size)
                    .ok()
                    .and_then(|size| size.parse().ok()),
            });
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_stat_lines_parse_with_spaces_in_the_path() {
        let (kind, size, mtime, real) = parse_stat(b"file 12 1700000000 /work/a b.txt\n").unwrap();
        assert_eq!(kind, EntryKind::File);
        assert_eq!((size, mtime), (12, 1_700_000_000));
        assert_eq!(real, "/work/a b.txt");
    }

    #[test]
    fn the_bundle_keeps_the_base_unwritten_and_names_the_layer() {
        let config = SandboxConfig {
            runsc: "/usr/bin/runsc".into(),
            helper: None,
            state: "/state".into(),
            rootfs: "/images/base".into(),
            toolstore: Some("/opt/tools".into()),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            shell: "/bin/bash".into(),
            network: false,
            rootless: false,
            flags: Vec::new(),
        };
        let spec = bundle_spec(&config, Some(Path::new("/s/layer.tar")));
        assert_eq!(spec["root"]["path"], "/images/base");
        assert_eq!(spec["annotations"][UPPER_ANNOTATION], "/s/layer.tar");
        assert_eq!(spec["process"]["env"][0], "PATH=/usr/bin:/bin");
        let toolstore = spec["mounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mount| mount["destination"] == "/opt/toolstore")
            .unwrap();
        assert_eq!(toolstore["options"], json!(["rbind", "ro"]));
        let without = bundle_spec(&config, None);
        assert!(without["annotations"].as_object().unwrap().is_empty());
    }
}
