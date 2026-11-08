//! Compute mode: the session's container in its project's tenantd sandbox.
//!
//! tenantd runs every sandbox on this host. Faber only names what it wants:
//!
//! - a **tenant** per user (`u-<user>`), whose storage limit is the user's
//!   scratch quota and covers everything below;
//! - a **sandbox** per project (`p-<project>`), started at the first
//!   promotion and destroyed once its last container is gone;
//! - two **volumes** per project, its workspace and its `/tmp`, the only
//!   state that outlives a container. Virtual mode works in the same volumes
//!   through tenantd's `Files` API ([`TenantdVolume`]), so a file written in
//!   one mode is there in the other, and nothing of tenantd's storage has to
//!   be on this host;
//! - a **container** per session in compute, bound to those volumes through
//!   the profile's `workspace` and `tmp` slots and to `/mnt` through its FUSE
//!   slot, which this process serves over `ServeFuse`. Its root is the
//!   image under a writable layer of its own, dropped when the container is
//!   deleted at idle.
//!
//! Every command, background process and file operation is a tenantd process
//! in that container. File operations go through tenantd's `Files` API, which
//! answers as the container sees its files, so they never depend on the
//! image's coreutils.
//!
//! Nothing of this process's environment reaches a container: a process
//! starts with the image's environment plus the call's, exactly the list the
//! virtual shell runs with.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use environment::files::{Confined, DirEntry, Files};
use environment::spawn::{Proc, Run, Sink, Source};
use environment::{Denial, EntryKind, Fault, Outcome, RootedPath, Signal, Stat};
use tenantd_client::files as tenantd_files;
use tenantd_client::pb;
use tenantd_client::{
    ContainersClient, ProcessesClient, SandboxesClient, TenantsClient, VolumesClient,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::sync::OnceCell;
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::fuse::MntFs;
use crate::mount::{ErrorLog, MountTable};
use crate::volume::{Meta, TenantdVolume, Volume};

/// The profile slots a scratch container binds its project's volumes to.
pub const WORKSPACE_SLOT: &str = "workspace";
pub const TMP_SLOT: &str = "tmp";

/// How long attaching the `/mnt` server to a container may take.
const FUSE_READY: Duration = Duration::from_secs(30);

/// One wait call's slice; waiting longer loops, so no single call outlives
/// tenantd's own deadline.
const WAIT_SLICE: Duration = Duration::from_secs(30);

/// How much of a stream the pipe between tenantd and the machine buffers.
const PIPE: usize = 64 * 1024;

/// Where tenantd's API is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// Its Unix socket, on this host. The process's UID must be one of
    /// tenantd's `server.callers`.
    Socket(PathBuf),
    /// Its `[server.tls]` listener, with mutual TLS.
    Tls(TlsEndpoint),
}

/// tenantd over TCP with mutual TLS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsEndpoint {
    /// `host:port`.
    pub address: String,
    /// The name tenantd's certificate must carry, when not the address's host.
    pub server_name: Option<String>,
    /// PEM CA certificates tenantd's certificate must chain to.
    pub ca: PathBuf,
    /// PEM client certificate chain, leaf first, and its key.
    pub cert: PathBuf,
    pub key: PathBuf,
}

impl Endpoint {
    /// The endpoint `var` describes: `{prefix}SOCKET`, or `{prefix}ENDPOINT`
    /// (`host:port`) with `{prefix}TLS_CA`, `{prefix}TLS_CERT` and
    /// `{prefix}TLS_KEY`, and optionally `{prefix}TLS_SERVER_NAME`. `None`
    /// when neither is set; blank is unset. The error says what is missing.
    pub fn from_vars(
        prefix: &str,
        var: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Endpoint>, String> {
        let set = |name: &str| {
            var(&format!("{prefix}{name}"))
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        match (set("SOCKET"), set("ENDPOINT")) {
            (None, None) => Ok(None),
            (Some(_), Some(_)) => Err(format!(
                "{prefix}SOCKET and {prefix}ENDPOINT are both set; tenantd is one or the other"
            )),
            (Some(socket), None) => Ok(Some(Endpoint::Socket(socket.into()))),
            (None, Some(address)) => {
                let (Some(ca), Some(cert), Some(key)) =
                    (set("TLS_CA"), set("TLS_CERT"), set("TLS_KEY"))
                else {
                    return Err(format!(
                        "{prefix}ENDPOINT needs {prefix}TLS_CA, {prefix}TLS_CERT and {prefix}TLS_KEY"
                    ));
                };
                Ok(Some(Endpoint::Tls(TlsEndpoint {
                    address,
                    server_name: set("TLS_SERVER_NAME"),
                    ca: ca.into(),
                    cert: cert.into(),
                    key: key.into(),
                })))
            }
        }
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Endpoint::Socket(path) => write!(f, "{}", path.display()),
            Endpoint::Tls(tls) => write!(f, "{} (mTLS)", tls.address),
        }
    }
}

/// How tenantd is reached and what scratch asks of it.
#[derive(Clone, Debug)]
pub struct TenantdConfig {
    pub endpoint: Endpoint,
    /// The sandbox profile every scratch sandbox runs under. It must declare
    /// the `workspace` and `tmp` slots and a FUSE slot at `/mnt`, and allow
    /// exec.
    pub profile: String,
    /// The scratch image, as a registry reference the profile admits.
    pub image: String,
    /// What one user's tenant may hold in total: workspaces, `/tmp`s,
    /// container roots and process output.
    pub storage_bytes: u64,
}

impl TenantdConfig {
    pub fn new(endpoint: Endpoint, image: impl Into<String>) -> Self {
        TenantdConfig {
            endpoint,
            profile: "scratch".to_owned(),
            image: image.into(),
            storage_bytes: 10 << 30,
        }
    }
}

/// The tenantd the ignored compute tests run against: `FABER_TEST_TENANTD`,
/// its socket, or `FABER_TEST_TENANTD_ENDPOINT` with the `_TLS_*` files, as
/// for [`Endpoint::from_vars`]. Panics on a half-set mTLS endpoint.
pub fn test_endpoint() -> Option<Endpoint> {
    if let Some(socket) = std::env::var_os("FABER_TEST_TENANTD") {
        return Some(Endpoint::Socket(socket.into()));
    }
    Endpoint::from_vars("FABER_TEST_TENANTD_", |name| std::env::var(name).ok())
        .unwrap_or_else(|reason| panic!("{reason}"))
}

/// What tenantd calls the objects behind one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Names {
    pub tenant: String,
    pub sandbox: String,
    pub container: String,
    pub workspace: String,
    pub tmp: String,
}

impl Names {
    /// Faber's ids are UUIDs; tenantd takes `[a-z0-9_-]{1,64}`.
    pub fn new(user: &str, project: &str, session: &str) -> Option<Names> {
        let id = |prefix: &str, raw: &str, suffix: &str| {
            let id = format!("{prefix}{}{suffix}", raw.to_ascii_lowercase());
            (id.len() <= 64
                && id.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'
                }))
            .then_some(id)
        };
        Some(Names {
            tenant: id("u-", user, "")?,
            sandbox: id("p-", project, "")?,
            container: id("", session, "")?,
            workspace: id("p-", project, "-workspace")?,
            tmp: id("p-", project, "-tmp")?,
        })
    }
}

/// A failure to reach tenantd or have it do something, as opposed to a
/// command failing in a container.
fn unreachable(what: &str, error: impl std::fmt::Display) -> Fault {
    Fault::Unreachable(format!("scratch sandbox: {what}: {error}"))
}

fn status(what: &str, status: Status) -> Fault {
    match status.code() {
        Code::ResourceExhausted => Fault::Denied(Denial::Malformed {
            what: "scratch".into(),
            reason: format!("{what}: {}", status.message()),
        }),
        _ => unreachable(what, format!("{:?}: {}", status.code(), status.message())),
    }
}

/// tenantd, over one lazily made connection.
pub struct Tenantd {
    config: TenantdConfig,
    channel: tokio::sync::Mutex<Option<Channel>>,
}

impl Tenantd {
    pub fn new(config: TenantdConfig) -> Self {
        Tenantd {
            config,
            channel: tokio::sync::Mutex::new(None),
        }
    }

    pub fn config(&self) -> &TenantdConfig {
        &self.config
    }

    async fn channel(&self) -> Result<Channel, Fault> {
        let mut channel = self.channel.lock().await;
        if let Some(channel) = &*channel {
            return Ok(channel.clone());
        }
        let what = format!("connecting to tenantd at {}", self.config.endpoint);
        let made = match &self.config.endpoint {
            Endpoint::Socket(path) => tenantd_client::connect(path)
                .await
                .map_err(|error| unreachable(&what, error))?,
            Endpoint::Tls(tls) => {
                let client =
                    tenantd_client::tls::ClientTls::from_pem_files(&tls.ca, &tls.cert, &tls.key)
                        .map_err(|error| unreachable(&what, error))?;
                tenantd_client::tls::connect(&tls.address, tls.server_name.as_deref(), &client)
                    .await
                    .map_err(|error| unreachable(&what, error))?
            }
        };
        *channel = Some(made.clone());
        Ok(made)
    }

    /// The user's tenant, created or brought to the configured limit.
    pub async fn ensure_tenant(&self, tenant: &str) -> Result<(), Fault> {
        let mut client = TenantsClient::new(self.channel().await?);
        let limits = pb::TenantLimits {
            storage_bytes: self.config.storage_bytes,
            cpus: None,
            memory_bytes: None,
        };
        let created = client
            .create_tenant(pb::CreateTenantRequest {
                id: tenant.to_owned(),
                limits: Some(limits),
            })
            .await;
        match created {
            Ok(_) => Ok(()),
            // There already, with other limits: the operator changed them.
            Err(refused) if refused.code() == Code::AlreadyExists => client
                .update_tenant(pb::UpdateTenantRequest {
                    id: tenant.to_owned(),
                    limits: Some(limits),
                })
                .await
                .map(|_| ())
                .map_err(|error| status("updating the user's tenant", error)),
            Err(error) => Err(status("creating the user's tenant", error)),
        }
    }

    /// The project's volumes, created if missing: (workspace, tmp).
    pub async fn ensure_volumes(
        &self,
        names: &Names,
    ) -> Result<(Arc<dyn Volume>, Arc<dyn Volume>), Fault> {
        self.ensure_tenant(&names.tenant).await?;
        let channel = self.channel().await?;
        let mut client = VolumesClient::new(channel.clone());
        let mut volumes: Vec<Arc<dyn Volume>> = Vec::new();
        for id in [&names.workspace, &names.tmp] {
            let created = client
                .create_volume(pb::CreateVolumeRequest {
                    tenant: names.tenant.clone(),
                    id: id.clone(),
                    // Root's, as the image's user is: virtual mode works in
                    // them as their owner, and so as the container does.
                    uid: None,
                    gid: None,
                    mode: Some(if id == &names.tmp { 0o1777 } else { 0o755 }),
                })
                .await;
            let volume = match created {
                Ok(volume) => volume.into_inner(),
                // There already, made with another owner or mode: it is the
                // project's all the same.
                Err(refused) if refused.code() == Code::AlreadyExists => client
                    .get_volume(pb::VolumeRef {
                        tenant: names.tenant.clone(),
                        id: id.clone(),
                    })
                    .await
                    .map_err(|error| status("looking up the project's volumes", error))?
                    .into_inner(),
                Err(error) => return Err(status("creating the project's volumes", error)),
            };
            if volume.state != "ready" {
                return Err(unreachable(
                    "creating the project's volumes",
                    format!("volume {id} is {}", volume.state),
                ));
            }
            volumes.push(Arc::new(TenantdVolume::new(
                channel.clone(),
                &names.tenant,
                id,
            )));
        }
        let tmp = volumes.pop().expect("two volumes");
        let workspace = volumes.pop().expect("two volumes");
        Ok((workspace, tmp))
    }

    /// Removes what is at or below `prefix` in a volume and has gone
    /// untouched for `older_than` (zero: all of it).
    pub async fn prune(
        &self,
        tenant: &str,
        volume: &str,
        prefix: &str,
        older_than: Duration,
    ) -> Result<(), Fault> {
        VolumesClient::new(self.channel().await?)
            .prune_volume(pb::PruneVolumeRequest {
                volume: Some(pb::VolumeRef {
                    tenant: tenant.to_owned(),
                    id: volume.to_owned(),
                }),
                path_prefix: prefix.to_owned(),
                older_than_secs: older_than.as_secs(),
            })
            .await
            .map(|_| ())
            .map_err(|error| status("pruning a volume", error))
    }

    /// Every `/tmp` volume scratch made: (tenant, volume id, the volume).
    pub async fn tmp_volumes(&self) -> Result<Vec<(String, String, Arc<dyn Volume>)>, Fault> {
        let channel = self.channel().await?;
        let tenants = TenantsClient::new(channel.clone())
            .list_tenants(pb::ListTenantsRequest {})
            .await
            .map_err(|error| status("listing tenants", error))?
            .into_inner()
            .tenants;
        let mut found = Vec::new();
        for tenant in tenants.into_iter().filter(|t| t.id.starts_with("u-")) {
            let volumes = VolumesClient::new(channel.clone())
                .list_volumes(pb::ListVolumesRequest {
                    tenant: tenant.id.clone(),
                })
                .await
                .map_err(|error| status("listing volumes", error))?
                .into_inner()
                .volumes;
            for volume in volumes {
                if volume.id.starts_with("p-") && volume.id.ends_with("-tmp") {
                    let files: Arc<dyn Volume> =
                        Arc::new(TenantdVolume::new(channel.clone(), &tenant.id, &volume.id));
                    found.push((tenant.id.clone(), volume.id, files));
                }
            }
        }
        Ok(found)
    }

    /// The project's sandbox, running.
    async fn ensure_sandbox(&self, names: &Names) -> Result<(), Fault> {
        let mut client = SandboxesClient::new(self.channel().await?);
        let reference = pb::SandboxRef {
            tenant: names.tenant.clone(),
            id: names.sandbox.clone(),
        };
        let sandbox = match client.get_sandbox(reference.clone()).await {
            Ok(found) => found.into_inner(),
            Err(missing) if missing.code() == Code::NotFound => client
                .create_sandbox(pb::CreateSandboxRequest {
                    tenant: names.tenant.clone(),
                    id: names.sandbox.clone(),
                    profile: self.config.profile.clone(),
                    image: self.config.image.clone(),
                    overrides: None,
                })
                .await
                .map_err(|error| status("creating the project's sandbox", error))?
                .into_inner(),
            Err(error) => return Err(status("looking up the project's sandbox", error)),
        };
        match sandbox.state.as_str() {
            "running" => Ok(()),
            "stopped" | "exited" => client
                .start_sandbox(reference)
                .await
                .map(|_| ())
                .map_err(|error| status("starting the project's sandbox", error)),
            other => Err(unreachable(
                "starting the project's sandbox",
                format!("it is {other}"),
            )),
        }
    }

    /// Starts the session's container: the project's sandbox and volumes
    /// first, then a fresh container bound to them, `/mnt` served by a
    /// [`MntFs`] over the session's mount table.
    pub async fn start(
        self: &Arc<Self>,
        names: Names,
        shell: &str,
        env: Vec<(String, String)>,
        private_tmp: PrivateTmp,
        mounts: Arc<MountTable>,
        errors: Arc<ErrorLog>,
    ) -> Result<Container, Fault> {
        self.ensure_volumes(&names).await?;
        self.ensure_sandbox(&names).await?;
        let mut client = ContainersClient::new(self.channel().await?);
        // Whatever is left of an earlier container of this session goes: a
        // container is never restarted, and its FUSE connection may have
        // been answered by a server that is gone.
        self.delete_container(&names).await;
        let created = client
            .create_container(pb::CreateContainerRequest {
                tenant: names.tenant.clone(),
                sandbox: names.sandbox.clone(),
                id: names.container.clone(),
                // Only the shell is assumed of the image; `sleep` is in
                // coreutils and busybox alike.
                args: vec![
                    shell.to_owned(),
                    "-c".to_owned(),
                    "while :; do sleep 86400; done".to_owned(),
                ],
                volumes: HashMap::from([
                    (WORKSPACE_SLOT.to_owned(), names.workspace.clone()),
                    (TMP_SLOT.to_owned(), names.tmp.clone()),
                ]),
                fuse: true,
                host_file_access: false,
            })
            .await
            .map_err(|error| status("creating the session's container", error))?
            .into_inner();
        let container = Container {
            tenantd: Arc::clone(self),
            names,
            shell: shell.to_owned(),
            env,
            private_tmp: Some(private_tmp),
            fuse: Mutex::new(None),
        };
        if created.fuse_path.is_empty() {
            tracing::warn!(
                container = %container.names.container,
                "the scratch profile has no FUSE slot; compute runs without /mnt"
            );
        } else if let Err(fault) = container.serve_mounts(mounts, errors).await {
            container.stop().await;
            return Err(fault);
        }
        if let Err(error) = client.start_container(container.reference()).await {
            container.stop().await;
            return Err(status("starting the session's container", error));
        }
        Ok(container)
    }

    async fn delete_container(&self, names: &Names) {
        let Ok(channel) = self.channel().await else {
            return;
        };
        let deleted = ContainersClient::new(channel)
            .delete_container(pb::ContainerRef {
                tenant: names.tenant.clone(),
                sandbox: names.sandbox.clone(),
                id: names.container.clone(),
            })
            .await;
        if let Err(error) = deleted
            && error.code() != Code::NotFound
        {
            tracing::warn!(container = %names.container, %error, "a scratch container was not deleted");
        }
    }

    /// Destroys the project's sandbox once it has no container left.
    async fn release_sandbox(&self, names: &Names) {
        let Ok(channel) = self.channel().await else {
            return;
        };
        let reference = pb::SandboxRef {
            tenant: names.tenant.clone(),
            id: names.sandbox.clone(),
        };
        let left = ContainersClient::new(channel.clone())
            .list_containers(pb::ListContainersRequest {
                tenant: names.tenant.clone(),
                sandbox: names.sandbox.clone(),
            })
            .await;
        match left {
            Ok(left) if left.get_ref().containers.is_empty() => {}
            _ => return,
        }
        if let Err(error) = SandboxesClient::new(channel)
            .destroy_sandbox(reference)
            .await
            && error.code() != Code::NotFound
        {
            tracing::warn!(sandbox = %names.sandbox, %error, "an empty scratch sandbox was not destroyed");
        }
    }

    /// Deletes every container and sandbox scratch left in tenantd, for a
    /// Faber process starting up: their processes and FUSE servers were the
    /// earlier process's.
    pub async fn recover(&self) {
        let Ok(channel) = self.channel().await else {
            tracing::warn!("tenantd is unreachable; scratch left nothing to clean up, or cannot");
            return;
        };
        let Ok(tenants) = TenantsClient::new(channel.clone())
            .list_tenants(pb::ListTenantsRequest {})
            .await
        else {
            return;
        };
        for tenant in tenants.into_inner().tenants {
            if !tenant.id.starts_with("u-") {
                continue;
            }
            let Ok(sandboxes) = SandboxesClient::new(channel.clone())
                .list_sandboxes(pb::ListSandboxesRequest {
                    tenant: tenant.id.clone(),
                })
                .await
            else {
                continue;
            };
            for sandbox in sandboxes.into_inner().sandboxes {
                if !sandbox.id.starts_with("p-") {
                    continue;
                }
                let names = Names {
                    tenant: tenant.id.clone(),
                    sandbox: sandbox.id.clone(),
                    container: String::new(),
                    workspace: String::new(),
                    tmp: format!("{}-tmp", sandbox.id),
                };
                if let Ok(containers) = ContainersClient::new(channel.clone())
                    .list_containers(pb::ListContainersRequest {
                        tenant: tenant.id.clone(),
                        sandbox: sandbox.id.clone(),
                    })
                    .await
                {
                    for container in containers.into_inner().containers {
                        let names = Names {
                            container: container.id.clone(),
                            ..names.clone()
                        };
                        self.delete_container(&names).await;
                        // Its session's private /tmp went with it.
                        if let Err(error) = self
                            .prune(
                                &names.tenant,
                                &names.tmp,
                                &private_dir(&container.id),
                                Duration::ZERO,
                            )
                            .await
                        {
                            tracing::warn!(container = %container.id, %error, "could not delete an orphan's /tmp");
                        }
                    }
                }
                self.release_sandbox(&names).await;
            }
        }
    }
}

/// A session's own directory in its project's `/tmp`: `$TMPDIR`.
#[derive(Clone)]
pub struct PrivateTmp {
    /// The project's `/tmp` volume.
    pub volume: Arc<dyn Volume>,
    /// Where it is in that volume.
    pub dir: String,
    /// Where it is in scratch, in both modes.
    pub path: String,
}

/// The name of a session's private directory in its project's `/tmp`.
pub fn private_dir(session: &str) -> String {
    format!("s-{session}")
}

/// A running container.
pub struct Container {
    tenantd: Arc<Tenantd>,
    names: Names,
    /// The shell the stdin redirect runs through.
    shell: String,
    /// What every process starts with, before the call's own.
    env: Vec<(String, String)>,
    /// None for an orphan, which only gets stopped.
    private_tmp: Option<PrivateTmp>,
    /// The task serving `/mnt`, while the container runs.
    fuse: Mutex<Option<tokio::task::AbortHandle>>,
}

impl Container {
    /// A container of this session that an earlier process may have left,
    /// held only to stop it.
    pub fn orphan(tenantd: Arc<Tenantd>, names: Names) -> Container {
        Container {
            tenantd,
            names,
            shell: String::new(),
            env: Vec::new(),
            private_tmp: None,
            fuse: Mutex::new(None),
        }
    }

    pub fn names(&self) -> &Names {
        &self.names
    }

    fn reference(&self) -> pb::ContainerRef {
        pb::ContainerRef {
            tenant: self.names.tenant.clone(),
            sandbox: self.names.sandbox.clone(),
            id: self.names.container.clone(),
        }
    }

    /// Attaches the `/mnt` server to the container's FUSE slot and serves it
    /// until the container ends. Returns once tenantd has the server, so the
    /// container can start.
    async fn serve_mounts(
        &self,
        mounts: Arc<MountTable>,
        errors: Arc<ErrorLog>,
    ) -> Result<(), Fault> {
        let channel = self.tenantd.channel().await?;
        let server = Arc::new(MntFs::new(mounts, errors));
        let session = tokio::time::timeout(
            FUSE_READY,
            tenantd_client::fuse::attach(channel, self.reference(), server, MntFs::limits()),
        )
        .await
        .map_err(|_| unreachable("serving /mnt", "attaching did not finish"))?
        .map_err(|error| status("serving /mnt", error))?;
        let container = self.names.container.clone();
        let task = tokio::spawn(async move {
            match session.run().await {
                Ok(()) => tracing::debug!(%container, "/mnt is no longer served"),
                Err(error) => {
                    tracing::warn!(%container, %error, "serving /mnt ended")
                }
            }
        });
        tracing::debug!(container = %self.names.container, "/mnt is served");
        *self.fuse.lock().expect("fuse poisoned") = Some(task.abort_handle());
        Ok(())
    }

    /// Starts one process in the container. It starts once its stdin is
    /// closed or its output is first read, whichever comes first, because
    /// tenantd takes no stdin yet: what was written to stdin by then is
    /// staged in the session's private `/tmp` and redirected from there.
    pub async fn spawn(self: &Arc<Self>, run: Run) -> Result<Box<dyn Proc>, Fault> {
        if run.pty {
            return Err(Fault::Denied(Denial::MissingCapability(
                environment::Capability::Pty,
            )));
        }
        let (stdout_tx, stdout) = tokio::io::duplex(PIPE);
        let (stderr_tx, stderr) = tokio::io::duplex(PIPE);
        let launch = Arc::new(Launch {
            container: Arc::clone(self),
            run,
            id: process_id(&self.names.container),
            stdin: Mutex::new(Some(Vec::new())),
            outputs: Mutex::new(Some((stdout_tx, stderr_tx))),
            started: OnceCell::new(),
        });
        Ok(Box::new(ContainerProc {
            stdin: Some(Box::new(StdinSink {
                launch: Arc::clone(&launch),
                closing: None,
            })),
            stdout: Some(Box::new(LazySource {
                inner: stdout,
                launch: Arc::clone(&launch),
                kicked: false,
            })),
            stderr: Some(Box::new(LazySource {
                inner: stderr,
                launch: Arc::clone(&launch),
                kicked: false,
            })),
            launch,
            outcome: None,
        }))
    }

    /// The container's own files, as its processes see them.
    async fn files(&self) -> Result<tenantd_files::Files, Fault> {
        Ok(tenantd_files::Files::new(
            self.tenantd.channel().await?,
            self.reference(),
        ))
    }

    /// Stages `body` in the session's private `/tmp` for a process to read:
    /// its path in the container.
    async fn stage(&self, what: &str, body: &[u8]) -> io::Result<String> {
        let Some(tmp) = &self.private_tmp else {
            return Err(io::Error::other("the container is being stopped"));
        };
        let name = format!(".faber-{what}-{}", next_serial());
        tmp.volume.mkdir(&tmp.dir, None, true).await?;
        tmp.volume
            .write(&format!("{}/{name}", tmp.dir), body, None)
            .await?;
        Ok(format!("{}/{name}", tmp.path))
    }

    /// Ends the container and everything in it, and with it the container's
    /// root. Destroys the project's sandbox when this was its last.
    pub async fn stop(&self) {
        self.tenantd.delete_container(&self.names).await;
        if let Some(task) = self.fuse.lock().expect("fuse poisoned").take() {
            task.abort();
        }
        self.tenantd.release_sandbox(&self.names).await;
    }
}

/// tenantd's process ids are unique across a sandbox and stay reserved for
/// a while after they are gone, so they never repeat, across restarts too.
fn process_id(container: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);
    format!("{container}-{millis:x}-{:x}", next_serial())
}

fn next_serial() -> u64 {
    static SERIAL: AtomicU64 = AtomicU64::new(1);
    SERIAL.fetch_add(1, Ordering::Relaxed)
}

type Outputs = (DuplexStream, DuplexStream);

/// A process not started until it is first needed.
struct Launch {
    container: Arc<Container>,
    run: Run,
    id: String,
    /// What was written to stdin before the start; `None` once started.
    stdin: Mutex<Option<Vec<u8>>>,
    outputs: Mutex<Option<Outputs>>,
    started: OnceCell<Result<(), Fault>>,
}

impl Launch {
    fn reference(&self) -> pb::ProcessRef {
        pb::ProcessRef {
            tenant: self.container.names.tenant.clone(),
            sandbox: self.container.names.sandbox.clone(),
            id: self.id.clone(),
        }
    }

    async fn started(self: &Arc<Self>) -> Result<(), Fault> {
        self.started
            .get_or_init(|| async { self.start().await })
            .await
            .clone()
    }

    async fn start(&self) -> Result<(), Fault> {
        let body = self
            .stdin
            .lock()
            .expect("stdin poisoned")
            .take()
            .unwrap_or_default();
        let mut args = self.run.argv.clone();
        if !body.is_empty() {
            let staged = self
                .container
                .stage("stdin", &body)
                .await
                .map_err(|error| unreachable("staging stdin", error))?;
            // `sh` only for the redirect; the command keeps its own argv.
            let mut wrapped = vec![
                self.container.shell.clone(),
                "-c".to_owned(),
                r#"exec 0<"$0" && rm -f -- "$0" && exec "$@""#.to_owned(),
                staged,
            ];
            wrapped.append(&mut args);
            args = wrapped;
        }
        let channel = self.container.tenantd.channel().await?;
        ProcessesClient::new(channel.clone())
            .start_process(pb::StartProcessRequest {
                tenant: self.container.names.tenant.clone(),
                sandbox: self.container.names.sandbox.clone(),
                id: self.id.clone(),
                args,
                env: self
                    .container
                    .env
                    .iter()
                    .chain(&self.run.env)
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect(),
                cwd: self.run.cwd.clone(),
                user: String::new(),
                tty: false,
                stdin: false,
                container: self.container.names.container.clone(),
            })
            .await
            .map_err(|error| status("starting a process", error))?;
        if let Some((stdout, stderr)) = self.outputs.lock().expect("outputs poisoned").take() {
            tokio::spawn(pump(
                channel.clone(),
                self.reference(),
                pb::Stream::Stdout,
                stdout,
            ));
            tokio::spawn(pump(channel, self.reference(), pb::Stream::Stderr, stderr));
        }
        Ok(())
    }
}

/// Copies one output stream of a process into `to` until the process has
/// exited and all of it is read.
async fn pump(channel: Channel, process: pb::ProcessRef, stream: pb::Stream, mut to: DuplexStream) {
    let mut client = ProcessesClient::new(channel);
    let mut offset = 0;
    let mut retries = 0;
    loop {
        let opened = client
            .read_output(pb::ReadOutputRequest {
                process: Some(process.clone()),
                stream: stream as i32,
                offset,
                follow: true,
            })
            .await;
        let mut chunks = match opened {
            Ok(chunks) => chunks.into_inner(),
            Err(error) if error.code() == Code::Unavailable && retries < 5 => {
                retries += 1;
                tokio::time::sleep(Duration::from_millis(200 << retries)).await;
                continue;
            }
            Err(error) => {
                tracing::warn!(process = %process.id, %error, "a process's output could not be read");
                let _ = to
                    .write_all(b"\n[scratch: output was cut short because reading it failed]\n")
                    .await;
                return;
            }
        };
        loop {
            match chunks.message().await {
                Ok(Some(pb::OutputChunk {
                    kind: Some(pb::output_chunk::Kind::Data(data)),
                })) => {
                    offset = data.offset + data.bytes.len() as u64;
                    if to.write_all(&data.bytes).await.is_err() {
                        return;
                    }
                }
                Ok(Some(pb::OutputChunk {
                    kind: Some(pb::output_chunk::Kind::Truncated(cut)),
                })) => {
                    let note = format!(
                        "\n[scratch: {} bytes of output past the limit were dropped]\n",
                        cut.dropped_bytes
                    );
                    let _ = to.write_all(note.as_bytes()).await;
                    return;
                }
                Ok(Some(_)) => {}
                Ok(None) => return,
                // A broken stream picks up where it stopped.
                Err(error) if error.code() == Code::Unavailable && retries < 5 => {
                    retries += 1;
                    break;
                }
                Err(error) => {
                    tracing::warn!(process = %process.id, %error, "a process's output stream broke");
                    let _ = to
                        .write_all(b"\n[scratch: output was cut short because reading it failed]\n")
                        .await;
                    return;
                }
            }
        }
    }
}

/// stdin of a process not yet started: kept until it starts, which closing
/// it does.
struct StdinSink {
    launch: Arc<Launch>,
    closing: Option<Starting>,
}

/// The start a closed stdin waits for.
type Starting = Pin<Box<dyn Future<Output = Result<(), Fault>> + Send>>;

impl AsyncWrite for StdinSink {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.launch.stdin.lock().expect("stdin poisoned").as_mut() {
            Some(body) => {
                body.extend_from_slice(buf);
                Poll::Ready(Ok(buf.len()))
            }
            None => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "a scratch process in compute takes stdin only before it starts",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.closing.is_none() {
            let launch = Arc::clone(&self.launch);
            self.closing = Some(Box::pin(async move { launch.started().await }));
        }
        let closing = self.closing.as_mut().expect("set above");
        closing
            .as_mut()
            .poll(cx)
            .map(|started| started.map_err(|fault| io::Error::other(fault.to_string())))
    }
}

/// An output stream that starts its process when first read.
struct LazySource {
    inner: DuplexStream,
    launch: Arc<Launch>,
    kicked: bool,
}

impl AsyncRead for LazySource {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.kicked {
            self.kicked = true;
            let launch = Arc::clone(&self.launch);
            tokio::spawn(async move {
                if launch.started().await.is_err() {
                    // Nothing will come: end both streams.
                    launch.outputs.lock().expect("outputs poisoned").take();
                }
            });
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

/// A process in a container.
struct ContainerProc {
    stdin: Option<Sink>,
    stdout: Option<Source>,
    stderr: Option<Source>,
    launch: Arc<Launch>,
    outcome: Option<Outcome>,
}

fn outcome_of(process: &pb::Process) -> Option<Outcome> {
    match process.state.as_str() {
        "exited" | "lost" => {}
        _ => return None,
    }
    if let Some(signal) = process.signal.and_then(|n| pb::Signal::try_from(n).ok())
        && let Some(signal) = from_pb(signal)
    {
        return Some(Outcome::Signaled { signal });
    }
    Some(Outcome::Completed {
        code: process.exit_code.unwrap_or(-1),
    })
}

fn from_pb(signal: pb::Signal) -> Option<Signal> {
    Some(match signal {
        pb::Signal::Sigterm => Signal::Term,
        pb::Signal::Sigkill => Signal::Kill,
        pb::Signal::Sigint => Signal::Int,
        pb::Signal::Sighup => Signal::Hup,
        pb::Signal::Sigquit => Signal::Quit,
        pb::Signal::Sigusr1 => Signal::Usr1,
        pb::Signal::Sigusr2 => Signal::Usr2,
        pb::Signal::Unspecified => return None,
    })
}

fn to_pb(signal: Signal) -> pb::Signal {
    match signal {
        Signal::Term => pb::Signal::Sigterm,
        Signal::Kill => pb::Signal::Sigkill,
        Signal::Int => pb::Signal::Sigint,
        Signal::Hup => pb::Signal::Sighup,
        Signal::Quit => pb::Signal::Sigquit,
        Signal::Usr1 => pb::Signal::Sigusr1,
        Signal::Usr2 => pb::Signal::Sigusr2,
    }
}

impl Launch {
    async fn client(&self) -> Result<ProcessesClient<Channel>, Fault> {
        Ok(ProcessesClient::new(
            self.container.tenantd.channel().await?,
        ))
    }
}

#[async_trait]
impl Proc for ContainerProc {
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
        let launch = Arc::clone(&self.launch);
        launch.started().await?;
        let mut client = launch.client().await?;
        loop {
            let process = client
                .wait_process(pb::WaitProcessRequest {
                    process: Some(launch.reference()),
                    timeout_ms: WAIT_SLICE.as_millis() as u64,
                })
                .await
                .map_err(|error| status("waiting for a process", error))?
                .into_inner();
            if let Some(outcome) = outcome_of(&process) {
                self.outcome = Some(outcome);
                return Ok(outcome);
            }
        }
    }

    async fn try_wait(&mut self) -> Result<Option<Outcome>, Fault> {
        if let Some(outcome) = self.outcome {
            return Ok(Some(outcome));
        }
        if self.launch.started.get().is_none() {
            return Ok(None);
        }
        let launch = Arc::clone(&self.launch);
        launch.started().await?;
        let process = launch
            .client()
            .await?
            .get_process(launch.reference())
            .await
            .map_err(|error| status("looking up a process", error))?
            .into_inner();
        self.outcome = outcome_of(&process);
        Ok(self.outcome)
    }

    async fn signal(&mut self, signal: Signal) -> Result<(), Fault> {
        if self.outcome.is_some() {
            return Err(Fault::Unreachable("the process handle is spent".to_owned()));
        }
        let launch = Arc::clone(&self.launch);
        launch.started().await?;
        launch
            .client()
            .await?
            .signal_process(pb::SignalProcessRequest {
                process: Some(launch.reference()),
                signal: to_pb(signal) as i32,
            })
            .await
            .map(|_| ())
            .map_err(|error| status("signalling a process", error))
    }
}

/// `read` and `patch` while scratch is in compute: the session's files are in
/// the container, reached through tenantd's `Files` API.
pub struct ContainerFiles {
    pub container: Arc<Container>,
}

fn file_fault(path: &RootedPath, error: tenantd_files::Error) -> Fault {
    if error.is_not_found() {
        return Fault::Denied(Denial::NotFound {
            path: path.to_string(),
        });
    }
    match error {
        tenantd_files::Error::Status(refused) => status("reaching the container's files", refused),
        other => unreachable("the container's files", other),
    }
}

fn entry_kind(attr: &pb::FileAttr) -> EntryKind {
    match attr.kind() {
        pb::FileKind::Directory => EntryKind::Dir,
        pb::FileKind::File => EntryKind::File,
        pb::FileKind::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

fn stat_of(path: &RootedPath, attr: &pb::FileAttr) -> Stat {
    Stat {
        path: path.clone(),
        size: attr.size,
        modified: Some(Meta::from_attr(attr).modified),
    }
}

#[async_trait]
impl Files for ContainerFiles {
    async fn confine(&self, path: &RootedPath) -> Result<Confined, Fault> {
        // The container is the boundary; all that is left is to say what is
        // there.
        match self
            .container
            .files()
            .await?
            .stat(path.as_str(), true)
            .await
        {
            Ok(found) => Ok(Confined {
                kind: Some(entry_kind(&found.attr)),
                path: found.path,
            }),
            Err(tenantd_files::Error::Errno { .. }) => Ok(Confined {
                path: path.as_str().to_owned(),
                kind: None,
            }),
            Err(error) => Err(file_fault(path, error)),
        }
    }

    async fn fetch(&self, path: &RootedPath) -> Result<Vec<u8>, Fault> {
        self.container
            .files()
            .await?
            .read(path.as_str())
            .await
            .map_err(|error| file_fault(path, error))
    }

    async fn store(&self, path: &RootedPath, body: &[u8]) -> Result<Stat, Fault> {
        let options = tenantd_files::WriteOptions {
            parents: true,
            ..Default::default()
        };
        let attr = self
            .container
            .files()
            .await?
            .write(path.as_str(), body.to_vec(), &options)
            .await
            .map_err(|error| file_fault(path, error))?;
        Ok(stat_of(path, &attr))
    }

    async fn remove(&self, path: &RootedPath) -> Result<(), Fault> {
        self.container
            .files()
            .await?
            .remove(path.as_str())
            .await
            .map_err(|error| file_fault(path, error))
    }

    async fn rename(&self, from: &RootedPath, to: &RootedPath) -> Result<Stat, Fault> {
        let mut files = self.container.files().await?;
        if let Some((parent, _)) = to.as_str().rsplit_once('/')
            && !parent.is_empty()
        {
            files
                .mkdir(parent, None, true)
                .await
                .map_err(|error| file_fault(to, error))?;
        }
        files
            .rename(from.as_str(), to.as_str(), 0)
            .await
            .map_err(|error| file_fault(from, error))?;
        let moved = files
            .stat(to.as_str(), true)
            .await
            .map_err(|error| file_fault(to, error))?;
        Ok(stat_of(to, &moved.attr))
    }

    async fn enumerate(&self, dir: &RootedPath) -> Result<Vec<DirEntry>, Fault> {
        let entries = self
            .container
            .files()
            .await?
            .read_dir(dir.as_str())
            .await
            .map_err(|error| file_fault(dir, error))?;
        Ok(entries
            .into_iter()
            .filter_map(|entry| {
                let attr = entry.attr?;
                Some(DirEntry {
                    kind: entry_kind(&attr),
                    size: Some(attr.size),
                    name: entry.name,
                })
            })
            .collect())
    }
}

/// Removes what in a project's `/tmp` volume lies outside every session's
/// private directory and has gone untouched for `older_than`. Each top-level
/// entry is pruned by `prune`, which removes what is at or below the name it
/// is given.
pub async fn sweep_tmp<F, Fut>(tmp: &dyn Volume, mut prune: F)
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let Ok(entries) = tmp.read_dir("/").await else {
        return;
    };
    for (name, _) in entries {
        if name.starts_with("s-") {
            continue;
        }
        prune(name).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooted(path: &str) -> RootedPath {
        RootedPath::new(&environment::Root::new("/").unwrap(), path).unwrap()
    }

    #[test]
    fn a_missing_file_is_not_found_and_other_errnos_are_faults() {
        let path = rooted("/work/a");
        let missing = tenantd_files::Error::Errno {
            errno: libc::ENOENT,
            detail: "gone".to_owned(),
        };
        assert!(matches!(
            file_fault(&path, missing),
            Fault::Denied(Denial::NotFound { .. })
        ));
        let directory = tenantd_files::Error::Errno {
            errno: libc::EISDIR,
            detail: "a directory".to_owned(),
        };
        assert!(matches!(
            file_fault(&path, directory),
            Fault::Unreachable(_)
        ));
    }

    #[test]
    fn a_quota_refusal_stays_a_denial() {
        let refused = tenantd_files::Error::Status(Status::resource_exhausted("over quota"));
        assert!(matches!(
            file_fault(&rooted("/work/a"), refused),
            Fault::Denied(Denial::Malformed { .. })
        ));
    }

    #[test]
    fn names_fit_what_tenantd_accepts() {
        let names = Names::new(
            "0199A7C2-0000-7000-8000-000000000001",
            "0199a7c2-0000-7000-8000-000000000002",
            "0199a7c2-0000-7000-8000-000000000003",
        )
        .unwrap();
        assert_eq!(names.tenant, "u-0199a7c2-0000-7000-8000-000000000001");
        assert_eq!(names.sandbox, "p-0199a7c2-0000-7000-8000-000000000002");
        assert_eq!(
            names.workspace,
            "p-0199a7c2-0000-7000-8000-000000000002-workspace"
        );
        assert_eq!(names.tmp, "p-0199a7c2-0000-7000-8000-000000000002-tmp");
        assert!(Names::new("a b", "p", "s").is_none());
        assert!(Names::new("u", &"p".repeat(60), "s").is_none());
        let id = process_id(&names.container);
        assert!(id.len() <= 64, "{id}");
    }

    #[test]
    fn an_exit_or_a_signal_is_an_outcome_and_a_live_process_is_none() {
        let mut process = pb::Process {
            state: "running".into(),
            ..Default::default()
        };
        assert_eq!(outcome_of(&process), None);
        process.state = "exited".into();
        process.exit_code = Some(3);
        assert_eq!(outcome_of(&process), Some(Outcome::Completed { code: 3 }));
        process.exit_code = None;
        process.signal = Some(pb::Signal::Sigkill as i32);
        assert_eq!(
            outcome_of(&process),
            Some(Outcome::Signaled {
                signal: Signal::Kill
            })
        );
    }
}
