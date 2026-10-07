//! Where calls run: a project's scratch and its machines, bound on demand and
//! kept, so a process started in one call is still there for the next one.
//!
//! The cache is module state and nothing more: an entry is rebuilt whenever
//! it is missing or its connection has changed. A process lives in the bound
//! target that started it, so it survives across calls and conversations for
//! as long as the binding does — not across a dropped agent connection or an
//! API restart, until the faber-agent side supervises processes itself.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime};

use diesel::{ExpressionMethods, OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::RunQueryDsl;
use environment::ssh::SshSession;
use environment::{BlobRef, Blobs, Exec, LocalTarget, Root, SshTarget, Target};
use uuid::Uuid;

use super::config::{MachineConfig, RepoRef, SCRATCH};
use crate::agent::AgentRegistry;
use crate::db::DbPool;
use crate::models::host::{Host, NewHostProbe, Transport};
use crate::schema::{host, host_probe};

/// How long reaching a machine may take before the call says it is
/// unreachable.
pub const REACH_TIMEOUT: Duration = Duration::from_secs(10);

/// Scratch unused this long is evicted whole.
pub const SCRATCH_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How often configured repos are checked for (and re-cloned after an
/// eviction) while scratch is in use.
const REPO_RECHECK: Duration = Duration::from_secs(60);

/// Why a call has nowhere to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// Unknown name, or `scratch` while disabled: the model's call.
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
    /// The connection this binding rides on; a different one means rebind.
    link: Option<Weak<SshSession>>,
    repos_checked: Mutex<Option<(Vec<RepoRef>, Instant)>>,
}

impl Bound {
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

/// Scratch's home on the Faber server.
///
/// Until the sandboxed scratch runtime lands, scratch is a directory per
/// project under `FABER_SCRATCH_ROOT`, and commands run as the API's own user
/// — which the manifest tells the agent, and which is why it is off unless an
/// operator turns it on.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    pub fn new(root: PathBuf) -> Self {
        Scratch { root }
    }

    fn dir(&self, project: Uuid) -> PathBuf {
        self.root.join("projects").join(project.to_string())
    }

    fn marker(&self, project: Uuid) -> PathBuf {
        self.root.join(".last-used").join(project.to_string())
    }

    async fn touch(&self, project: Uuid) {
        let marker = self.marker(project);
        if let Some(parent) = marker.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(&marker, chrono::Utc::now().to_rfc3339()).await;
    }
}

pub struct Places {
    db: DbPool,
    agents: Arc<AgentRegistry>,
    scratch: Option<Scratch>,
    cache: tokio::sync::Mutex<HashMap<(Uuid, String), Arc<Bound>>>,
}

impl Places {
    pub fn new(db: DbPool, agents: Arc<AgentRegistry>, scratch: Option<Scratch>) -> Self {
        Places {
            db,
            agents,
            scratch,
            cache: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn scratch_available(&self) -> bool {
        self.scratch.is_some()
    }

    /// Whether a machine's agent is connected right now. No call is made.
    pub fn online(&self, agent: Uuid) -> bool {
        self.agents.get(agent).is_some()
    }

    /// What is already bound for a project, without binding anything.
    pub async fn bound(&self, project: Uuid) -> Vec<Arc<Bound>> {
        self.cache
            .lock()
            .await
            .iter()
            .filter(|((owner, _), _)| *owner == project)
            .map(|(_, bound)| Arc::clone(bound))
            .collect()
    }

    pub async fn scratch(
        &self,
        project: Uuid,
        repos: &[RepoRef],
    ) -> Result<(Arc<Bound>, Vec<String>), Unavailable> {
        let Some(scratch) = &self.scratch else {
            return Err(Unavailable::Unreachable(
                "scratch is not available on this Faber server".to_owned(),
            ));
        };
        scratch.touch(project).await;

        let key = (project, SCRATCH.to_owned());
        let cached = self.cache.lock().await.get(&key).cloned();
        let bound = match cached {
            Some(bound) => bound,
            None => {
                let dir = scratch.dir(project);
                tokio::fs::create_dir_all(dir.join("repos"))
                    .await
                    .map_err(|error| {
                        Unavailable::Unreachable(format!("scratch could not be created: {error}"))
                    })?;
                let root = Root::new(dir.to_string_lossy())
                    .map_err(|denial| Unavailable::Unreachable(denial.to_string()))?;
                let blobs = Arc::new(RecentBlobs::default());
                let machine = tokio::time::timeout(
                    REACH_TIMEOUT,
                    LocalTarget::bind(SCRATCH, root, Arc::clone(&blobs) as Arc<dyn Blobs>),
                )
                .await
                .map_err(|_| Unavailable::Unreachable("scratch did not start in time".to_owned()))?
                .map_err(|fault| Unavailable::Unreachable(fault.to_string()))?;
                let bound = Arc::new(Bound {
                    name: SCRATCH.to_owned(),
                    target: Arc::new(machine),
                    generation: generation(),
                    blobs,
                    link: None,
                    repos_checked: Mutex::new(None),
                });
                self.cache.lock().await.insert(key, Arc::clone(&bound));
                bound
            }
        };

        let notes = ensure_repos(&bound, repos).await;
        Ok((bound, notes))
    }

    pub async fn machine(
        &self,
        project: Uuid,
        owner: Uuid,
        machine: &MachineConfig,
    ) -> Result<Arc<Bound>, Unavailable> {
        let unreachable =
            |why: String| Unavailable::Unreachable(format!("`{}` {why}", machine.name));

        let mut conn = self
            .db
            .get()
            .await
            .map_err(|error| unreachable(format!("could not be looked up: {error}")))?;
        let row: Option<Host> = host::table
            .filter(host::id.eq(machine.agent_id))
            .filter(host::user_id.eq(owner))
            .select(Host::as_select())
            .first(&mut conn)
            .await
            .optional()
            .map_err(|error| unreachable(format!("could not be looked up: {error}")))?;
        drop(conn);

        let Some(row) = row else {
            return Err(unreachable(
                "points at a faber-agent that is not one of this project owner's computers"
                    .to_owned(),
            ));
        };
        if row.transport != Transport::Agent.as_str() {
            return Err(unreachable("is not a faber-agent machine".to_owned()));
        }
        if row.disabled_at.is_some() {
            return Err(unreachable("is disabled".to_owned()));
        }
        let Some(session) = self.agents.get(row.id) else {
            return Err(unreachable(
                "is offline: its faber-agent is not connected".to_owned(),
            ));
        };

        let key = (project, machine.id.clone());
        if let Some(bound) = self.cache.lock().await.get(&key).cloned()
            && bound.name == machine.name
            && bound
                .link
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|link| Arc::ptr_eq(&link, &session))
        {
            return Ok(bound);
        }

        // Real paths: the root is the machine's own `/`, and `workdir` is only
        // where commands start.
        let root = Root::new("/").map_err(|denial| unreachable(denial.to_string()))?;
        let blobs = Arc::new(RecentBlobs::default());
        let target = tokio::time::timeout(
            REACH_TIMEOUT,
            SshTarget::bind_session(
                machine.name.clone(),
                Arc::clone(&session),
                root,
                Arc::clone(&blobs) as Arc<dyn Blobs>,
            ),
        )
        .await
        .map_err(|_| {
            unreachable(format!(
                "did not answer within {}s",
                REACH_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|fault| unreachable(fault.to_string()))?;

        self.record_probe(row.id, target.manifest()).await;

        let bound = Arc::new(Bound {
            name: machine.name.clone(),
            target: Arc::new(target),
            generation: generation(),
            blobs,
            link: Some(Arc::downgrade(&session)),
            repos_checked: Mutex::new(None),
        });
        self.cache.lock().await.insert(key, Arc::clone(&bound));
        Ok(bound)
    }

    /// The probe a bind just did is the freshest manifest there is; the
    /// system head and manifest notices read it from here without a live
    /// call.
    async fn record_probe(&self, host_id: Uuid, manifest: &environment::Manifest) {
        let Ok(mut conn) = self.db.get().await else {
            return;
        };
        let tools = serde_json::to_value(&manifest.tools).ok();
        let result = diesel::insert_into(host_probe::table)
            .values(&NewHostProbe {
                id: Uuid::now_v7(),
                host_id,
                container_id: None,
                ok: true,
                error: None,
                os: Some(&manifest.os),
                arch: Some(&manifest.arch),
                shell: Some(&manifest.shell),
                tools,
                root_path: Some(manifest.root.as_str()),
            })
            .execute(&mut conn)
            .await;
        if let Err(error) = result {
            tracing::warn!(%host_id, %error, "could not cache a machine probe");
        }
    }

    /// Evicts scratch unused for [`SCRATCH_RETENTION`]: the directory, its
    /// marker, and the bound target with every process in it.
    pub async fn evict_idle_scratch(&self) {
        let Some(scratch) = &self.scratch else { return };
        let Ok(mut markers) = tokio::fs::read_dir(scratch.root.join(".last-used")).await else {
            return;
        };
        while let Ok(Some(marker)) = markers.next_entry().await {
            let Ok(project) = marker.file_name().to_string_lossy().parse::<Uuid>() else {
                continue;
            };
            let idle = marker
                .metadata()
                .await
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|idle| idle > SCRATCH_RETENTION);
            if !idle {
                continue;
            }

            if let Some(bound) = self
                .cache
                .lock()
                .await
                .remove(&(project, SCRATCH.to_owned()))
            {
                if let Ok(processes) = bound.target.processes().await {
                    for process in processes.iter().filter(|process| process.outcome.is_none()) {
                        let _ = bound
                            .target
                            .signal(process.id, environment::Signal::Kill)
                            .await;
                    }
                }
            }
            let _ = tokio::fs::remove_dir_all(scratch.dir(project)).await;
            let _ = tokio::fs::remove_file(marker.path()).await;
            tracing::info!(%project, "evicted idle scratch");
        }
    }
}

/// Clones configured repos that are not there yet: on first use, after an
/// eviction, and when the configured list changes. What failed is returned
/// for the call to report; nothing here fails the call.
async fn ensure_repos(bound: &Bound, repos: &[RepoRef]) -> Vec<String> {
    {
        let checked = bound.repos_checked.lock().expect("repo check poisoned");
        if let Some((list, at)) = checked.as_ref()
            && list.as_slice() == repos
            && at.elapsed() < REPO_RECHECK
        {
            return Vec::new();
        }
    }

    let mut notes = Vec::new();
    for repo in repos {
        let dir = format!("/repos/{}", repo.dir);
        let mut script = format!(
            "test -e {dir}/.git && exit 0; rm -rf {dir} && git clone --quiet -- {url} {dir}",
            dir = shell_quote(&dir),
            url = shell_quote(&repo.url),
        );
        if let Some(reference) = &repo.reference {
            script.push_str(&format!(
                " && git -C {dir} checkout --quiet {reference}",
                dir = shell_quote(&dir),
                reference = shell_quote(reference),
            ));
        }
        let cwd = bound.target.root();
        let exec = Exec::new(script)
            .cwd(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .timeout(Duration::from_secs(300));
        match bound.target.exec(exec).await {
            Ok(exit) if matches!(exit.outcome, environment::Outcome::Completed { code: 0 }) => {}
            Ok(exit) => {
                let stderr = bound
                    .blobs
                    .get(&exit.stderr.span.blob)
                    .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_owned())
                    .unwrap_or_default();
                notes.push(format!(
                    "{} could not be cloned to {dir}: {stderr}",
                    repo.url
                ));
            }
            Err(fault) => notes.push(format!(
                "{} could not be cloned to {dir}: {fault}",
                repo.url
            )),
        }
    }

    *bound.repos_checked.lock().expect("repo check poisoned") =
        Some((repos.to_vec(), Instant::now()));
    notes
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
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
    entries: Mutex<(u64, VecDeque<(u64, Vec<u8>)>)>,
}

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
    fn shell_quoting_survives_a_quote() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }
}
