//! Where calls run: the session's scratch, and the user environments a
//! project's config names, bound on demand and kept, so a process started in
//! one call is still there for the next one.
//!
//! The cache is module state and nothing more: an entry is rebuilt whenever
//! it is missing, its config entry now names a different environment, or its
//! connection has changed. User environments are keyed by project and config
//! entry, so a process outlives the conversation that started it for as long
//! as the binding does — not across a dropped connection or an API restart.
//! Scratch is the runtime's, keyed by session (see [`super::runtime`]).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use environment::ssh::SshSession;
use environment::{BlobRef, Blobs, Target};
use uuid::Uuid;

use super::config::EnvironmentConfig;
use super::runtime::ScratchRuntime;
use crate::agent::AgentRegistry;
use crate::db::DbPool;
use crate::environments::{Registered, bind_registered, registered};
use crate::error::AppError;
use crate::models::host::{HostProbe, NewHostProbe, Transport};
use crate::schema::{host, host_probe};
use crate::state::AppState;

/// How long reaching a machine may take before the call says it is
/// unreachable.
pub const REACH_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a call has nowhere to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// Unknown name, or an entry that is turned off: the model's call.
    Missing(String),
    /// Offline, or not reachable from here: nobody's call.
    Unreachable(String),
}

/// A bound target and what calls against it need alongside.
pub struct Bound {
    pub name: String,
    pub target: Arc<dyn Target>,
    /// Marks this binding's process ids, so an id from an earlier binding of
    /// the same environment is unknown rather than someone else's process.
    pub generation: String,
    pub blobs: Arc<RecentBlobs>,
    /// Reached over SSH, which has no standing connection to report on: a
    /// call that loses it says so in its own result.
    pub over_ssh: bool,
    /// The registered environment this binding is, for a user environment.
    environment_id: Option<Uuid>,
    /// What it was built from ([`Registered::source`]); a binding whose
    /// environment has changed since is made again.
    source: Option<String>,
    /// The connection this binding rides on; a different one means rebind.
    link: Option<Weak<SshSession>>,
}

impl Bound {
    /// A binding that rides on no connection of its own — what a scratch
    /// runtime hands back.
    pub fn new(name: impl Into<String>, target: Arc<dyn Target>, blobs: Arc<RecentBlobs>) -> Self {
        Bound {
            name: name.into(),
            target,
            generation: generation(),
            blobs,
            over_ssh: false,
            environment_id: None,
            source: None,
            link: None,
        }
    }

    pub fn process_id(&self, id: environment::ProcId) -> String {
        format!("{}-{}", self.generation, id.0)
    }

    pub fn parse_process_id(&self, id: &str) -> Option<environment::ProcId> {
        let (generation, number) = id.rsplit_once('-')?;
        if generation != self.generation {
            return None;
        }
        number.parse().ok().map(environment::ProcId)
    }
}

pub struct Places {
    db: DbPool,
    agents: Arc<AgentRegistry>,
    /// What binding a registered environment needs: the SSH pool, the
    /// credential store's key, the agent registry. Set once at startup, after
    /// the state that holds this module exists; the cycle that makes is
    /// deliberate and lives as long as the process.
    state: OnceLock<AppState>,
    scratch: Arc<dyn ScratchRuntime>,
    cache: tokio::sync::Mutex<HashMap<(Uuid, String), Arc<Bound>>>,
}

impl Places {
    pub fn new(db: DbPool, agents: Arc<AgentRegistry>, scratch: Arc<dyn ScratchRuntime>) -> Self {
        Places {
            db,
            agents,
            state: OnceLock::new(),
            scratch,
            cache: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Hands over the application state user environments are bound with.
    /// Later calls are ignored: there is one state per process.
    pub fn attach(&self, state: AppState) {
        let _ = self.state.set(state);
    }

    /// The application state, once attached.
    pub fn state(&self) -> Option<&AppState> {
        self.state.get()
    }

    pub fn scratch(&self) -> &Arc<dyn ScratchRuntime> {
        &self.scratch
    }

    /// Whether an agent-transport host is connected right now. No call is
    /// made.
    pub fn online(&self, host_id: Uuid) -> bool {
        self.agents.get(host_id).is_some()
    }

    /// The registered environments behind these ids, among the owner's.
    pub async fn registered(&self, owner: Uuid, ids: &[Uuid]) -> Vec<Registered> {
        let Ok(mut conn) = self.db.get().await else {
            return Vec::new();
        };
        registered(&mut conn, owner, ids).await.unwrap_or_default()
    }

    /// The latest successful probe of each environment, keyed by the id the
    /// config names it by. Never a live call.
    pub async fn probes(&self, owner: Uuid, found: &[Registered]) -> HashMap<Uuid, HostProbe> {
        let mut probes = HashMap::new();
        let Ok(mut conn) = self.db.get().await else {
            return probes;
        };
        let hosts: Vec<Uuid> = found
            .iter()
            .filter(|registered| registered.container.is_none())
            .map(|registered| registered.host.id)
            .collect();
        let containers: Vec<Uuid> = found
            .iter()
            .filter_map(|registered| registered.container.as_ref().map(|container| container.id))
            .collect();

        if !hosts.is_empty() {
            let rows: Vec<HostProbe> = host_probe::table
                .inner_join(host::table)
                .filter(host::user_id.eq(owner))
                .filter(host_probe::host_id.eq_any(&hosts))
                .filter(host_probe::container_id.is_null())
                .filter(host_probe::ok.eq(true))
                .distinct_on(host_probe::host_id)
                .order((host_probe::host_id, host_probe::probed_at.desc()))
                .select(HostProbe::as_select())
                .load(&mut conn)
                .await
                .unwrap_or_default();
            probes.extend(rows.into_iter().map(|probe| (probe.host_id, probe)));
        }
        if !containers.is_empty() {
            let rows: Vec<HostProbe> = host_probe::table
                .inner_join(host::table)
                .filter(host::user_id.eq(owner))
                .filter(host_probe::container_id.eq_any(&containers))
                .filter(host_probe::ok.eq(true))
                .distinct_on(host_probe::container_id)
                .order((host_probe::container_id, host_probe::probed_at.desc()))
                .select(HostProbe::as_select())
                .load(&mut conn)
                .await
                .unwrap_or_default();
            probes.extend(
                rows.into_iter()
                    .filter_map(|probe| probe.container_id.map(|id| (id, probe))),
            );
        }
        probes
    }

    /// Binds (or reuses) the environment a `user` entry names.
    pub async fn user(
        &self,
        project: Uuid,
        owner: Uuid,
        entry: &EnvironmentConfig,
    ) -> Result<Arc<Bound>, Unavailable> {
        let name = &entry.name;
        let unreachable = |why: String| Unavailable::Unreachable(format!("`{name}` {why}"));
        let Some(environment_id) = entry.environment_id else {
            return Err(unreachable("names no registered environment".to_owned()));
        };
        let Some(state) = self.state.get() else {
            return Err(unreachable(
                "cannot be reached yet: this server is still starting".to_owned(),
            ));
        };

        let mut conn = self
            .db
            .get()
            .await
            .map_err(|error| unreachable(format!("could not be looked up: {error}")))?;
        let found = registered(&mut conn, owner, &[environment_id])
            .await
            .map_err(|error| unreachable(format!("could not be looked up: {error}")))?;
        drop(conn);
        let Some(found) = found.into_iter().next() else {
            return Err(unreachable(
                "points at an environment that is not one of this project owner's".to_owned(),
            ));
        };
        if found.host.disabled_at.is_some() {
            return Err(unreachable(
                "is disabled in the user's environment settings".to_owned(),
            ));
        }
        let transport = found.transport();
        let live = match transport {
            Some(Transport::Agent) => match self.agents.get(found.host.id) {
                Some(session) => Some(session),
                None => {
                    return Err(unreachable(
                        "is offline: its faber-agent is not connected".to_owned(),
                    ));
                }
            },
            _ => None,
        };

        let key = (project, entry.id.clone());
        if let Some(bound) = self.cache.lock().await.get(&key).cloned()
            && bound.name == *name
            && bound.environment_id == Some(environment_id)
            && bound.source.as_deref() == Some(found.source().as_str())
            && fresh(&bound, live.as_ref())
        {
            return Ok(bound);
        }

        let over_ssh = transport == Some(Transport::Ssh);
        let failed = |reason: String| {
            if over_ssh {
                unreachable(format!("is unreachable over SSH: {reason}"))
            } else {
                unreachable(format!("could not be reached: {reason}"))
            }
        };
        let blobs = Arc::new(RecentBlobs::default());
        let reached = tokio::time::timeout(
            REACH_TIMEOUT,
            bind_registered(
                state,
                owner,
                name,
                &found,
                Arc::clone(&blobs) as Arc<dyn Blobs>,
                true,
            ),
        )
        .await
        .map_err(|_| {
            failed(format!(
                "it did not answer within {}s",
                REACH_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|error| failed(reason(&error)))?;

        self.record_probe(&found, reached.machine.manifest()).await;

        let bound = Arc::new(Bound {
            name: name.clone(),
            target: Arc::new(reached.machine),
            generation: generation(),
            blobs,
            over_ssh,
            environment_id: Some(environment_id),
            source: Some(found.source()),
            link: reached.link.as_ref().map(Arc::downgrade),
        });
        self.cache.lock().await.insert(key, Arc::clone(&bound));
        Ok(bound)
    }

    /// The probe a bind just did is the freshest description there is; the
    /// system head and notices read it from here without a live call.
    async fn record_probe(&self, found: &Registered, manifest: &environment::Manifest) {
        let Ok(mut conn) = self.db.get().await else {
            return;
        };
        let tools = serde_json::to_value(&manifest.tools).ok();
        let result = diesel::insert_into(host_probe::table)
            .values(&NewHostProbe {
                id: Uuid::now_v7(),
                host_id: found.host.id,
                container_id: found.container.as_ref().map(|container| container.id),
                ok: true,
                error: None,
                os: Some(&manifest.os),
                arch: Some(&manifest.arch),
                shell: Some(&manifest.shell),
                tools,
                root_path: Some(manifest.root.as_str()),
                home: Some(&manifest.home),
            })
            .execute(&mut conn)
            .await;
        if let Err(error) = result {
            tracing::warn!(host_id = %found.host.id, %error, "could not cache an environment probe");
        }
    }
}

/// Whether a cached binding still rides on the connection that would be used
/// now. `live` is the agent's current session for an agent host; an SSH
/// binding is stale once its pooled session closes; a local one never is.
fn fresh(bound: &Bound, live: Option<&Arc<SshSession>>) -> bool {
    match (&bound.link, live) {
        (None, None) => true,
        (Some(link), Some(live)) => link.upgrade().is_some_and(|link| Arc::ptr_eq(&link, live)),
        (Some(link), None) => link.upgrade().is_some_and(|link| !link.is_closed()),
        (None, Some(_)) => false,
    }
}

/// The part of an error worth saying after "unreachable over SSH:".
fn reason(error: &AppError) -> String {
    match error {
        AppError::ServiceUnavailable(reason)
        | AppError::BadRequest(reason)
        | AppError::BadGateway(reason)
        | AppError::Conflict(reason) => reason.clone(),
        AppError::NotFound => "it is no longer registered".to_owned(),
        other => other.to_string(),
    }
}

fn generation() -> String {
    format!("{:06x}", rand::random::<u32>() & 0xff_ffff)
}

/// A blob store that keeps the most recent entries only.
///
/// Spans are redeemed while a result is rendered, right after they are made;
/// a bound target lives far longer than that, so an unbounded store would
/// hold every byte any command ever printed.
#[derive(Default)]
pub struct RecentBlobs {
    entries: Mutex<Recent>,
}

/// The next id to hand out, and the newest blobs by id.
type Recent = (u64, VecDeque<(u64, Vec<u8>)>);

const RECENT_BLOBS: usize = 256;

impl Blobs for RecentBlobs {
    fn put(&self, bytes: &[u8]) -> BlobRef {
        let mut entries = self.entries.lock().expect("blob store poisoned");
        let id = entries.0;
        entries.0 += 1;
        entries.1.push_back((id, bytes.to_vec()));
        while entries.1.len() > RECENT_BLOBS {
            entries.1.pop_front();
        }
        BlobRef(format!("recent:{id}"))
    }

    fn get(&self, blob: &BlobRef) -> Option<Vec<u8>> {
        let id: u64 = blob.0.strip_prefix("recent:")?.parse().ok()?;
        let entries = self.entries.lock().expect("blob store poisoned");
        entries
            .1
            .iter()
            .find(|(entry, _)| *entry == id)
            .map(|(_, bytes)| bytes.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_blobs_keep_the_newest() {
        let blobs = RecentBlobs::default();
        let first = blobs.put(b"first");
        let mut last = first.clone();
        for _ in 0..RECENT_BLOBS {
            last = blobs.put(b"later");
        }
        assert!(blobs.get(&first).is_none());
        assert_eq!(blobs.get(&last).unwrap(), b"later");
    }

    #[test]
    fn an_ssh_failure_reads_as_its_reason() {
        assert_eq!(
            reason(&AppError::ServiceUnavailable("connection refused".into())),
            "connection refused"
        );
    }

    #[test]
    fn process_ids_belong_to_one_binding() {
        let blobs = Arc::new(RecentBlobs::default());
        let target = Arc::new(stub()) as Arc<dyn Target>;
        let one = Bound::new("a", Arc::clone(&target), Arc::clone(&blobs));
        let two = Bound::new("a", target, blobs);
        let id = one.process_id(environment::ProcId(7));
        assert_eq!(one.parse_process_id(&id), Some(environment::ProcId(7)));
        // Generations are random; equal ones would make this vacuous.
        if one.generation != two.generation {
            assert_eq!(two.parse_process_id(&id), None);
        }
        assert!(fresh(&one, None));
    }

    fn stub() -> environment::Machine {
        let dir = std::env::temp_dir();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(environment::LocalTarget::bind(
                "a",
                environment::Root::new(dir.to_string_lossy()).unwrap(),
                Arc::new(environment::MemoryBlobs::new()),
            ))
            .unwrap()
    }
}
