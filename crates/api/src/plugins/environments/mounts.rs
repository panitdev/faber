//! Mounts in a session's scratch: the declarations Environments keeps, the
//! host side of `faber:environments/mounts`, and the adapter that turns a
//! file operation under `/mnt` into a call to the owning plugin's provider.
//!
//! A mount is a live view: its files are served by the plugin that declared
//! it, through `faber:environments/provider`, and are never copied or
//! archived. All Environments keeps is the declaration (plugin, name,
//! read-only), stored with the session so a restored scratch can re-establish
//! it. Environments chooses where it goes, `/mnt/<plugin-id>/<name>`, and the
//! plugin id comes from the caller's context, never from an argument — so no
//! plugin can cover another plugin's mount, `/`, or scratch's own files.
//!
//! Changes to one session's declarations are serialized by a per-session
//! lock: the cap check and the write it guards cannot interleave with another
//! mount in the same session, and handing the live scratch its mounts reads
//! the declarations and serves them under that same lock, so a stale list is
//! never served after a newer one.
//!
//! A plugin turned off keeps its declarations, unserved while it is off; only
//! removing it from the project drops them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use diesel::upsert::excluded;
use diesel::{ExpressionMethods, QueryDsl, SelectableHelper};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use plugin::environments::{
    self as contract, Entry, FsError, MountError, MountSpec, Mounts, ProviderRoutes, mount_path,
    normalize_path,
};
use plugin::{Message, Notices, SessionCtx};
use uuid::Uuid;

use super::config::EnvironmentsConfig;
use super::places::Places;
use super::runtime::LiveMount;
use crate::db::DbPool;
use crate::models::project::ProjectBinding;
use crate::schema::{project_binding, session, session_mount};

/// Mounts one session may hold, across all its plugins. Each is a provider
/// the filesystem layer dispatches to, and each is listed in the head, so the
/// list stays short enough to read.
pub const MOUNT_CAP: usize = 32;

/// Whether a session's project has scratch to mount into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scratch {
    /// The project's environments list has no scratch entry.
    Missing,
    /// It has one, turned off.
    Disabled,
    Enabled,
}

/// One stored mount declaration.
#[derive(Clone, Debug, PartialEq, Eq, diesel::Queryable, diesel::Selectable)]
#[diesel(table_name = session_mount)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Declaration {
    pub plugin_id: String,
    pub name: String,
    pub read_only: bool,
}

impl Declaration {
    /// Where it appears in scratch.
    pub fn path(&self) -> String {
        mount_path(&self.plugin_id, &self.name)
    }
}

#[derive(diesel::Insertable)]
#[diesel(table_name = session_mount)]
struct NewMount<'a> {
    session_id: Uuid,
    plugin_id: &'a str,
    name: &'a str,
    read_only: bool,
    created_at: DateTime<Utc>,
}

/// What a successful `mount` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mounted {
    pub declaration: Declaration,
    /// The declaration this one replaced, when the name was already mounted.
    pub replaced: Option<Declaration>,
}

/// What restoring a session's mounts left.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// Still declared, to re-establish, in mount order.
    pub kept: Vec<Declaration>,
    /// Paths dropped because their plugin is no longer in the project. Each
    /// is worth a notice; see [`gone_notice`].
    pub gone: Vec<String>,
}

/// Why a mount operation failed: an answer the caller is owed, or the store
/// itself failing.
#[derive(Debug, thiserror::Error)]
pub enum MountFailure {
    #[error(transparent)]
    Refused(#[from] MountError),
    #[error("mount store: {0}")]
    Store(String),
}

impl From<diesel::result::Error> for MountFailure {
    fn from(error: diesel::result::Error) -> Self {
        MountFailure::Store(error.to_string())
    }
}

impl From<bb8::RunError<diesel_async::pooled_connection::PoolError>> for MountFailure {
    fn from(error: bb8::RunError<diesel_async::pooled_connection::PoolError>) -> Self {
        MountFailure::Store(error.to_string())
    }
}

/// A mount name: the same rule as an environment name, because it becomes a
/// single path segment the agent types — letters, digits, `.`, `_`, `-`,
/// starting with a letter or digit, at most 64 bytes. Starting with a letter
/// or digit also rules out `.` and `..`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The store of mount declarations, owned by the Environments plugin.
pub struct MountService {
    db: DbPool,
    cap: usize,
    /// One lock per session that has changed its mounts in this process.
    /// Entries are never removed: a lock is a few bytes, and dropping one
    /// while a waiter holds a clone would let two writers in.
    locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

impl MountService {
    pub fn new(db: DbPool) -> Self {
        MountService::with_cap(db, MOUNT_CAP)
    }

    pub fn with_cap(db: DbPool, cap: usize) -> Self {
        MountService {
            db,
            cap,
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// The session's lock. Not reentrant: nothing that takes it may be
    /// called while it is held.
    async fn lock(&self, session: Uuid) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = Arc::clone(
            self.locks
                .lock()
                .expect("mount lock table poisoned")
                .entry(session)
                .or_default(),
        );
        lock.lock_owned().await
    }

    /// Declares `plugin`'s mount `spec.name` in `session`'s scratch, replacing
    /// a mount of the same name. Returns the declaration as stored.
    pub async fn mount(
        &self,
        session: Uuid,
        plugin: &str,
        scratch: Scratch,
        spec: &MountSpec,
    ) -> Result<Mounted, MountFailure> {
        match scratch {
            Scratch::Missing => return Err(MountError::NoScratch.into()),
            Scratch::Disabled => return Err(MountError::ScratchDisabled.into()),
            Scratch::Enabled => {}
        }
        if !valid_name(&spec.name) {
            return Err(MountError::InvalidName.into());
        }

        let _guard = self.lock(session).await;
        let mut conn = self.db.get().await?;
        let existing: Vec<Declaration> = session_mount::table
            .filter(session_mount::session_id.eq(session))
            .select(Declaration::as_select())
            .load(&mut conn)
            .await?;
        let replaced = existing
            .iter()
            .find(|d| d.plugin_id == plugin && d.name == spec.name)
            .cloned();
        if replaced.is_none() && existing.len() >= self.cap {
            return Err(MountError::TooMany.into());
        }

        // A replacement keeps its original `created_at`, so remounting does
        // not move a mount in the head's list.
        diesel::insert_into(session_mount::table)
            .values(NewMount {
                session_id: session,
                plugin_id: plugin,
                name: &spec.name,
                read_only: spec.read_only,
                created_at: Utc::now(),
            })
            .on_conflict((
                session_mount::session_id,
                session_mount::plugin_id,
                session_mount::name,
            ))
            .do_update()
            .set(session_mount::read_only.eq(excluded(session_mount::read_only)))
            .execute(&mut conn)
            .await?;

        Ok(Mounted {
            declaration: Declaration {
                plugin_id: plugin.to_owned(),
                name: spec.name.clone(),
                read_only: spec.read_only,
            },
            replaced,
        })
    }

    /// Removes `plugin`'s mount `name` from `session`. Allowed whether or not
    /// scratch is on: dropping a declaration is always possible.
    pub async fn unmount(
        &self,
        session: Uuid,
        plugin: &str,
        name: &str,
    ) -> Result<Declaration, MountFailure> {
        if !valid_name(name) {
            return Err(MountError::InvalidName.into());
        }
        let _guard = self.lock(session).await;
        let mut conn = self.db.get().await?;
        let removed: Option<Declaration> = diesel::delete(
            session_mount::table
                .filter(session_mount::session_id.eq(session))
                .filter(session_mount::plugin_id.eq(plugin))
                .filter(session_mount::name.eq(name)),
        )
        .returning(Declaration::as_returning())
        .get_result(&mut conn)
        .await
        .map(Some)
        .or_else(|error| match error {
            diesel::result::Error::NotFound => Ok(None),
            other => Err(other),
        })?;
        removed.ok_or(MountError::NotFound.into())
    }

    /// A session's mounts in the order they were first made: what the
    /// scratch line in the head lists.
    pub async fn list(&self, session: Uuid) -> Result<Vec<Declaration>, MountFailure> {
        let mut conn = self.db.get().await?;
        Ok(session_mount::table
            .filter(session_mount::session_id.eq(session))
            .order((
                session_mount::created_at,
                session_mount::plugin_id,
                session_mount::name,
            ))
            .select(Declaration::as_select())
            .load(&mut conn)
            .await?)
    }

    /// Re-establishes a session's declarations when its scratch is restored.
    /// A mount whose plugin is not in `bound` (the plugins in the project
    /// now, on or off) is dropped, and its path returned so the agent can be
    /// told it is gone. The caller holds the session's lock.
    pub async fn restore(&self, session: Uuid, bound: &[String]) -> Result<Restored, MountFailure> {
        let declarations = self.list(session).await?;
        let (kept, gone): (Vec<_>, Vec<_>) = declarations
            .into_iter()
            .partition(|d| bound.iter().any(|plugin| plugin == &d.plugin_id));
        if !gone.is_empty() {
            let mut conn = self.db.get().await?;
            let plugins: Vec<&str> = gone.iter().map(|d| d.plugin_id.as_str()).collect();
            diesel::delete(
                session_mount::table
                    .filter(session_mount::session_id.eq(session))
                    .filter(session_mount::plugin_id.eq_any(plugins)),
            )
            .execute(&mut conn)
            .await?;
        }
        Ok(Restored {
            kept,
            gone: gone.iter().map(Declaration::path).collect(),
        })
    }
}

/// Drops `plugin`'s mounts across every session of `project`, when the plugin
/// is removed from it. Returns each session and the paths it lost.
///
/// Run in the transaction that removes the binding, so the declarations and
/// the binding go together: no restart in between can leave either behind.
/// One statement, so it needs no session's lock; a mount racing it in
/// another session is dropped on that session's next restore, as its plugin
/// is no longer in the project.
pub async fn drop_plugin(
    conn: &mut AsyncPgConnection,
    project: Uuid,
    plugin: &str,
) -> Result<Vec<(Uuid, Vec<String>)>, diesel::result::Error> {
    let sessions = session::table
        .filter(session::project_id.eq(project))
        .select(session::id);
    let removed: Vec<(Uuid, String)> = diesel::delete(
        session_mount::table
            .filter(session_mount::plugin_id.eq(plugin))
            .filter(session_mount::session_id.eq_any(sessions)),
    )
    .returning((session_mount::session_id, session_mount::name))
    .get_results(conn)
    .await?;

    let mut by_session: Vec<(Uuid, Vec<String>)> = Vec::new();
    for (session, name) in removed {
        let path = mount_path(plugin, &name);
        match by_session.iter_mut().find(|(s, _)| *s == session) {
            Some((_, paths)) => paths.push(path),
            None => by_session.push((session, vec![path])),
        }
    }
    for (_, paths) in &mut by_session {
        paths.sort();
    }
    Ok(by_session)
}

// --- What the agent is told ---

fn notice(text: String) -> Message {
    Message::user(format!("<environments>{text}</environments>"))
}

fn described(declaration: &Declaration) -> String {
    if declaration.read_only {
        format!("`{}` (read-only)", declaration.path())
    } else {
        format!("`{}`", declaration.path())
    }
}

/// The mounts part of the scratch line in the head; `None` with no mounts.
pub fn head_mounts(declarations: &[Declaration]) -> Option<String> {
    if declarations.is_empty() {
        return None;
    }
    Some(format!(
        "Mounted in scratch: {}.",
        declarations
            .iter()
            .map(described)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// A mount added or replaced mid-session.
pub fn mounted_notice(mounted: &Mounted) -> Message {
    let what = described(&mounted.declaration);
    notice(match &mounted.replaced {
        Some(_) => format!("{what} in scratch was mounted again and now reflects its new source."),
        None => format!("{what} is now mounted in scratch: a live view, not a copy."),
    })
}

/// A mount removed by its plugin mid-session.
pub fn unmounted_notice(declaration: &Declaration) -> Message {
    notice(format!(
        "`{}` was unmounted from scratch.",
        declaration.path()
    ))
}

/// Mounts dropped because their plugin left the project.
pub fn gone_notice(paths: &[String]) -> Option<Message> {
    if paths.is_empty() {
        return None;
    }
    let list = paths
        .iter()
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(notice(if paths.len() == 1 {
        format!("{list} is gone from scratch: its plugin was removed from this project.")
    } else {
        format!("{list} are gone from scratch: their plugin was removed from this project.")
    }))
}

// --- faber:environments/mounts, host side ---

/// What serving `mounts` needs from the rest of Environments: whether the
/// session has scratch, a way to bring the session's live scratch in step
/// with its declarations, and a way to tell the agent about a change in
/// Environments' own name (not the mounting plugin's).
#[async_trait]
pub trait MountsContext: Send + Sync + 'static {
    async fn scratch(&self, session: Uuid) -> Result<Scratch, String>;
    async fn sync(&self, session: Uuid);
    async fn notify(&self, session: Uuid, notice: Message);
}

/// The host-side implementation of `faber:environments/mounts`, handed to
/// built-ins that import it. The caller is the binding its context belongs
/// to.
///
/// A failure of the store itself has no `mount-error` to name it; as a host
/// failure during a component's call would trap that call, it panics here,
/// which the dispatcher turns into a `handler-failure` for the tool call that
/// was mounting.
pub struct MountsHost {
    service: Arc<MountService>,
    context: Arc<dyn MountsContext>,
}

impl MountsHost {
    pub fn new(service: Arc<MountService>, context: Arc<dyn MountsContext>) -> Self {
        MountsHost { service, context }
    }
}

/// The session a context belongs to. A context whose session id is not one
/// Faber issued is not in a project, so it has no scratch.
fn session_of(ctx: &SessionCtx) -> Result<Uuid, MountError> {
    ctx.session_id().parse().map_err(|_| MountError::NoScratch)
}

fn trap<T>(result: Result<T, MountFailure>) -> Result<T, MountError> {
    match result {
        Ok(value) => Ok(value),
        Err(MountFailure::Refused(error)) => Err(error),
        Err(MountFailure::Store(error)) => panic!("mounts: {error}"),
    }
}

#[async_trait]
impl Mounts for MountsHost {
    async fn mount(&self, ctx: &SessionCtx, spec: MountSpec) -> Result<String, MountError> {
        let session = session_of(ctx)?;
        let scratch = match self.context.scratch(session).await {
            Ok(scratch) => scratch,
            Err(error) => panic!("mounts: {error}"),
        };
        let mounted = trap(
            self.service
                .mount(session, ctx.binding(), scratch, &spec)
                .await,
        )?;
        if mounted.replaced.as_ref() != Some(&mounted.declaration) {
            self.context.sync(session).await;
            self.context.notify(session, mounted_notice(&mounted)).await;
        }
        Ok(mounted.declaration.path())
    }

    async fn unmount(&self, ctx: &SessionCtx, name: &str) -> Result<(), MountError> {
        let session = session_of(ctx)?;
        let removed = trap(self.service.unmount(session, ctx.binding(), name).await)?;
        self.context.sync(session).await;
        self.context
            .notify(session, unmounted_notice(&removed))
            .await;
        Ok(())
    }
}

// --- Under /mnt: from a path to a provider call ---

/// Serves a session's mounts to the filesystem layer and the file tools:
/// the seven provider operations, addressed by (plugin, mount, path), routed
/// to the plugin that owns the mount and called in that plugin's name.
///
/// Scratch's own mount table resolves a path under `/mnt` to its (plugin,
/// mount) and lists `/mnt` and `/mnt/<plugin>`; it reaches this backend
/// through one [`OneMount`] per declaration, all sharing it.
///
/// Read-only mounts are enforced here, before the provider is called, so a
/// provider never has to. A mount that is not declared, or whose plugin is no
/// longer bound and enabled, is `not-found`.
pub struct MountBackend {
    session: Uuid,
    routes: ProviderRoutes,
    notices: Arc<Notices>,
    mounts: RwLock<Vec<Declaration>>,
}

impl MountBackend {
    pub fn new(
        session: Uuid,
        routes: ProviderRoutes,
        notices: Arc<Notices>,
        mounts: Vec<Declaration>,
    ) -> Self {
        MountBackend {
            session,
            routes,
            notices,
            mounts: RwLock::new(mounts),
        }
    }

    /// Replaces the declarations served. Live sessions get a new backend
    /// on every change instead; this is for checking what one backend does.
    #[cfg(test)]
    pub fn set_mounts(&self, mounts: Vec<Declaration>) {
        *self.mounts.write().expect("mount table poisoned") = mounts;
    }

    #[cfg(test)]
    pub fn mounts_for_test(&self) -> Vec<Declaration> {
        self.mounts.read().expect("mount table poisoned").clone()
    }

    fn declared(&self, plugin: &str, mount: &str) -> Result<Declaration, FsError> {
        self.mounts
            .read()
            .expect("mount table poisoned")
            .iter()
            .find(|d| d.plugin_id == plugin && d.name == mount)
            .cloned()
            .ok_or(FsError::NotFound)
    }

    /// The provider to call and the context to call it with, after checking
    /// the mount is declared and, for a write, writable.
    fn route(
        &self,
        plugin: &str,
        mount: &str,
        write: bool,
    ) -> Result<(contract::SharedProvider, SessionCtx), FsError> {
        let declaration = self.declared(plugin, mount)?;
        if write && declaration.read_only {
            return Err(FsError::ReadOnly);
        }
        self.routes
            .route(plugin, &self.session.to_string(), &self.notices)
            .ok_or(FsError::NotFound)
    }

    fn inner(path: &str) -> Result<String, FsError> {
        normalize_path(path).ok_or(FsError::Denied)
    }

    pub async fn stat(&self, plugin: &str, mount: &str, path: &str) -> Result<Entry, FsError> {
        let (provider, ctx) = self.route(plugin, mount, false)?;
        provider.stat(&ctx, mount, &Self::inner(path)?).await
    }

    pub async fn read_dir(
        &self,
        plugin: &str,
        mount: &str,
        path: &str,
    ) -> Result<Vec<Entry>, FsError> {
        let (provider, ctx) = self.route(plugin, mount, false)?;
        provider.read_dir(&ctx, mount, &Self::inner(path)?).await
    }

    pub async fn read(
        &self,
        plugin: &str,
        mount: &str,
        path: &str,
        offset: u64,
        max: u32,
    ) -> Result<Vec<u8>, FsError> {
        let (provider, ctx) = self.route(plugin, mount, false)?;
        provider
            .read(&ctx, mount, &Self::inner(path)?, offset, max)
            .await
    }

    pub async fn put(
        &self,
        plugin: &str,
        mount: &str,
        path: &str,
        data: Vec<u8>,
        if_version: Option<String>,
        create_only: bool,
    ) -> Result<Entry, FsError> {
        let (provider, ctx) = self.route(plugin, mount, true)?;
        let path = Self::inner(path)?;
        if path == "/" {
            return Err(FsError::IsADirectory);
        }
        provider
            .put(&ctx, mount, &path, data, if_version, create_only)
            .await
    }

    pub async fn mkdir(&self, plugin: &str, mount: &str, path: &str) -> Result<Entry, FsError> {
        let (provider, ctx) = self.route(plugin, mount, true)?;
        let path = Self::inner(path)?;
        if path == "/" {
            return Err(FsError::Exists);
        }
        provider.mkdir(&ctx, mount, &path).await
    }

    /// Within one mount only: a move across mounts is a copy the caller does
    /// with `read` and `put`.
    pub async fn rename(
        &self,
        plugin: &str,
        mount: &str,
        src: &str,
        dst: &str,
    ) -> Result<(), FsError> {
        let (provider, ctx) = self.route(plugin, mount, true)?;
        let (src, dst) = (Self::inner(src)?, Self::inner(dst)?);
        if src == "/" || dst == "/" {
            // The mount's root belongs to Environments, not the provider.
            return Err(FsError::Denied);
        }
        provider.rename(&ctx, mount, &src, &dst).await
    }

    pub async fn remove(
        &self,
        plugin: &str,
        mount: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), FsError> {
        let (provider, ctx) = self.route(plugin, mount, true)?;
        let path = Self::inner(path)?;
        if path == "/" {
            return Err(FsError::Denied);
        }
        provider.remove(&ctx, mount, &path, recursive).await
    }
}

/// One mount of a session's scratch, as scratch's filesystem layer calls
/// it: the session-wide [`MountBackend`] narrowed to one (plugin, mount), with
/// paths relative to the mount's root.
pub struct OneMount {
    all: Arc<MountBackend>,
    plugin: String,
    mount: String,
}

#[async_trait]
impl scratch::MountBackend for OneMount {
    async fn stat(&self, path: &str) -> Result<Entry, FsError> {
        self.all.stat(&self.plugin, &self.mount, path).await
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<Entry>, FsError> {
        self.all.read_dir(&self.plugin, &self.mount, path).await
    }

    async fn read(&self, path: &str, offset: u64, max: u32) -> Result<Vec<u8>, FsError> {
        self.all
            .read(&self.plugin, &self.mount, path, offset, max)
            .await
    }

    async fn put(
        &self,
        path: &str,
        data: Vec<u8>,
        if_version: Option<String>,
        create_only: bool,
    ) -> Result<Entry, FsError> {
        self.all
            .put(
                &self.plugin,
                &self.mount,
                path,
                data,
                if_version,
                create_only,
            )
            .await
    }

    async fn mkdir(&self, path: &str) -> Result<Entry, FsError> {
        self.all.mkdir(&self.plugin, &self.mount, path).await
    }

    async fn rename(&self, src: &str, dst: &str) -> Result<(), FsError> {
        self.all.rename(&self.plugin, &self.mount, src, dst).await
    }

    async fn remove(&self, path: &str, recursive: bool) -> Result<(), FsError> {
        self.all
            .remove(&self.plugin, &self.mount, path, recursive)
            .await
    }
}

// --- Keeping a session's live scratch in step with its declarations ---

/// The `declarations` whose plugin is turned on among `bindings`.
fn enabled(bindings: &[ProjectBinding], declarations: Vec<Declaration>) -> Vec<Declaration> {
    declarations
        .into_iter()
        .filter(|declaration| {
            bindings
                .iter()
                .any(|binding| binding.enabled && binding.plugin_type == declaration.plugin_id)
        })
        .collect()
}

/// Environments' side of mounts: the declarations, the scratch they are
/// served in, and what the agent is told. Every change goes the same way:
/// store the declarations, then hand the session's open scratch the whole
/// current set, then post the notice.
pub struct SessionMounts {
    service: Arc<MountService>,
    places: Arc<Places>,
    db: DbPool,
}

impl SessionMounts {
    pub fn new(db: DbPool, places: Arc<Places>) -> Self {
        SessionMounts {
            service: Arc::new(MountService::new(db.clone())),
            places,
            db,
        }
    }

    pub fn service(&self) -> &Arc<MountService> {
        &self.service
    }

    /// The bindings of the session's project.
    async fn bindings(&self, session: Uuid) -> Result<Vec<ProjectBinding>, String> {
        let mut conn = self.db.get().await.map_err(|error| error.to_string())?;
        let project: Option<Uuid> = session::table
            .filter(session::id.eq(session))
            .select(session::project_id)
            .first(&mut conn)
            .await
            .map_err(|error| error.to_string())?;
        let Some(project) = project else {
            return Ok(Vec::new());
        };
        project_binding::table
            .filter(project_binding::project_id.eq(project))
            .select(ProjectBinding::as_select())
            .load(&mut conn)
            .await
            .map_err(|error| error.to_string())
    }

    /// Re-establishes a session's mounts when its scratch is opened: drops
    /// the declarations of plugins no longer in the project, serves those of
    /// the plugins turned on, and returns the notice for what is gone.
    pub async fn restore(&self, session: Uuid) -> Option<Message> {
        let _guard = self.service.lock(session).await;
        let bindings = match self.bindings(session).await {
            Ok(bindings) => bindings,
            Err(error) => {
                tracing::warn!(%session, %error, "scratch mounts not restored");
                return None;
            }
        };
        let present: Vec<String> = bindings
            .iter()
            .map(|binding| binding.plugin_type.clone())
            .collect();
        let restored = match self.service.restore(session, &present).await {
            Ok(restored) => restored,
            Err(error) => {
                tracing::warn!(%session, %error, "scratch mounts not restored");
                return None;
            }
        };
        self.serve(session, &bindings, restored.kept).await;
        // Only a scratch call restores, so scratch is on.
        gone_notice(&restored.gone)
    }

    /// The declarations the session serves: those of plugins turned on in
    /// its project, in mount order. What the head lists.
    pub async fn served(&self, session: Uuid) -> Vec<Declaration> {
        let (Ok(bindings), Ok(declarations)) = (
            self.bindings(session).await,
            self.service.list(session).await,
        ) else {
            return Vec::new();
        };
        enabled(&bindings, declarations)
    }

    /// `plugin` was removed from `project` and `lost` is what
    /// [`drop_plugin`] dropped: each session that lost mounts serves what is
    /// left and is told.
    pub async fn plugin_removed(&self, lost: Vec<(Uuid, Vec<String>)>) {
        for (session, paths) in lost {
            self.sync(session).await;
            if let Some(notice) = gone_notice(&paths) {
                self.notify(session, notice).await;
            }
        }
    }

    /// A plugin of `project` was turned on or off: every session of the
    /// project this process has open serves what is now on. Nothing is
    /// dropped and nobody is told; the head lists what is served.
    pub async fn plugin_toggled(&self, project: Uuid) {
        let open = self.places.scratch().open_sessions().await;
        if open.is_empty() {
            return;
        }
        let Ok(mut conn) = self.db.get().await else {
            return;
        };
        let sessions: Vec<Uuid> = match session::table
            .filter(session::project_id.eq(project))
            .select(session::id)
            .load(&mut conn)
            .await
        {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%project, %error, "scratch mounts not brought in step");
                return;
            }
        };
        drop(conn);
        for session in sessions {
            if open.contains(&session.to_string()) {
                self.sync(session).await;
            }
        }
    }

    /// Hands the session's open scratch the `declarations` of plugins turned
    /// on, served through the providers its project routes to now. The
    /// caller holds the session's lock.
    async fn serve(
        &self,
        session: Uuid,
        bindings: &[ProjectBinding],
        declarations: Vec<Declaration>,
    ) {
        let Some(state) = self.places.state() else {
            return;
        };
        let declarations = enabled(bindings, declarations);
        let all: Vec<plugin::Binding> = bindings.iter().map(ProjectBinding::to_binding).collect();
        let routes = ProviderRoutes::new(&state.plugins.registry, &all);
        let backend = Arc::new(MountBackend::new(
            session,
            routes,
            state.plugins.notices(session),
            declarations.clone(),
        ));
        let live = declarations
            .into_iter()
            .map(|declaration| LiveMount {
                backend: Arc::new(OneMount {
                    all: Arc::clone(&backend),
                    plugin: declaration.plugin_id.clone(),
                    mount: declaration.name.clone(),
                }),
                plugin: declaration.plugin_id,
                name: declaration.name,
                read_only: declaration.read_only,
            })
            .collect();
        self.places
            .scratch()
            .set_mounts(&session.to_string(), live)
            .await;
    }
}

#[async_trait]
impl MountsContext for SessionMounts {
    async fn scratch(&self, session: Uuid) -> Result<Scratch, String> {
        let bindings = self.bindings(session).await?;
        let Some(binding) = bindings
            .iter()
            .find(|binding| binding.plugin_type == super::ID && binding.enabled)
        else {
            return Ok(Scratch::Missing);
        };
        let stored = binding.to_binding();
        let config = super::migrate(stored.config, stored.config_version)
            .ok()
            .and_then(|config| EnvironmentsConfig::parse(&config).ok())
            .ok_or_else(|| "the environments config could not be read".to_owned())?;
        Ok(match config.scratch() {
            None => Scratch::Missing,
            Some(entry) if !entry.enabled => Scratch::Disabled,
            Some(_) => Scratch::Enabled,
        })
    }

    async fn sync(&self, session: Uuid) {
        let _guard = self.service.lock(session).await;
        let bindings = match self.bindings(session).await {
            Ok(bindings) => bindings,
            Err(error) => {
                tracing::warn!(%session, %error, "scratch mounts not brought in step");
                return;
            }
        };
        match self.service.list(session).await {
            Ok(declarations) => self.serve(session, &bindings, declarations).await,
            Err(error) => tracing::warn!(%session, %error, "scratch mounts not brought in step"),
        }
    }

    /// Posts only while the session's scratch is on, and only to a session
    /// this process already keeps a queue for: one with no queue has not run
    /// here, and its next run's head lists its mounts as they are.
    async fn notify(&self, session: Uuid, notice: Message) {
        let Some(state) = self.places.state() else {
            return;
        };
        if !matches!(self.scratch(session).await, Ok(Scratch::Enabled)) {
            return;
        }
        let Some(notices) = state.plugins.existing_notices(session) else {
            return;
        };
        let order = self
            .bindings(session)
            .await
            .ok()
            .and_then(|bindings| {
                bindings
                    .iter()
                    .find(|binding| binding.plugin_type == super::ID)
                    .map(|binding| binding.created_at.timestamp_micros())
            })
            .unwrap_or(0);
        if let Err(error) = notices.post(
            super::ID,
            order,
            notice,
            plugin::Delivery::AfterToolOrMessage,
        ) {
            tracing::warn!(%session, %error, "mounts notice refused");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use plugin::bind::Binding;
    use plugin::environments::{Kind, PROVIDER, Provider, SharedProvider};
    use plugin::{
        ConfigError, Content, Json, Manifest, Phase, Plugin, Registry, ToolCall, ToolResult,
    };
    use serde_json::json;

    #[test]
    fn mount_names_follow_the_environment_name_rule() {
        for good in ["docs", "a", "v1.2", "my_repo-2", &"x".repeat(64)] {
            assert!(valid_name(good), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "-x",
            "a/b",
            "a b",
            "ü",
            &"x".repeat(65),
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn notices_and_the_head_name_the_paths() {
        let docs = Declaration {
            plugin_id: "notion".into(),
            name: "docs".into(),
            read_only: true,
        };
        let notes = Declaration {
            plugin_id: "notion".into(),
            name: "notes".into(),
            read_only: false,
        };
        assert_eq!(
            head_mounts(&[docs.clone(), notes.clone()]).unwrap(),
            "Mounted in scratch: `/mnt/notion/docs` (read-only), `/mnt/notion/notes`."
        );
        assert_eq!(head_mounts(&[]), None);
        let added = mounted_notice(&Mounted {
            declaration: notes.clone(),
            replaced: None,
        })
        .text();
        assert!(added.starts_with("<environments>`/mnt/notion/notes` is now mounted"));
        assert_eq!(
            gone_notice(&["/mnt/notion/docs".into()]).unwrap().text(),
            "<environments>`/mnt/notion/docs` is gone from scratch: its plugin was removed from this project.</environments>"
        );
        assert!(gone_notice(&[]).is_none());
        assert!(unmounted_notice(&docs).text().contains("unmounted"));
    }

    // --- The backend, against a built-in that serves one file per mount. ---

    struct Echo;

    #[async_trait]
    impl Provider for Echo {
        async fn stat(&self, ctx: &SessionCtx, mount: &str, path: &str) -> Result<Entry, FsError> {
            if path == "/missing" {
                return Err(FsError::NotFound);
            }
            Ok(Entry {
                name: format!("{}:{}:{mount}:{path}", ctx.binding(), ctx.session_id()),
                kind: if path == "/" { Kind::Dir } else { Kind::File },
                size: 0,
                mtime: 0,
                version: "1".into(),
            })
        }
        async fn read_dir(&self, _: &SessionCtx, _: &str, _: &str) -> Result<Vec<Entry>, FsError> {
            Ok(Vec::new())
        }
        async fn read(
            &self,
            _: &SessionCtx,
            mount: &str,
            path: &str,
            _: u64,
            _: u32,
        ) -> Result<Vec<u8>, FsError> {
            Ok(format!("{mount}{path}").into_bytes())
        }
        async fn put(
            &self,
            _: &SessionCtx,
            _: &str,
            path: &str,
            data: Vec<u8>,
            _: Option<String>,
            _: bool,
        ) -> Result<Entry, FsError> {
            Ok(Entry {
                name: path.into(),
                kind: Kind::File,
                size: data.len() as u64,
                mtime: 0,
                version: "2".into(),
            })
        }
        async fn mkdir(&self, _: &SessionCtx, _: &str, _: &str) -> Result<Entry, FsError> {
            Err(FsError::Denied)
        }
        async fn rename(&self, _: &SessionCtx, _: &str, _: &str, _: &str) -> Result<(), FsError> {
            Ok(())
        }
        async fn remove(&self, _: &SessionCtx, _: &str, _: &str, _: bool) -> Result<(), FsError> {
            Ok(())
        }
    }

    struct Mounter(Manifest);

    #[async_trait]
    impl Plugin for Mounter {
        fn manifest(&self) -> &Manifest {
            &self.0
        }
        fn exports(&self) -> Vec<String> {
            vec![PROVIDER.into()]
        }
        fn provider(&self) -> Option<SharedProvider> {
            Some(Arc::new(Echo))
        }
        async fn open(&self, _: &SessionCtx, _: Phase, _: &Json) -> Result<Vec<Content>, String> {
            Ok(Vec::new())
        }
        async fn handle_tool(&self, _: &SessionCtx, _: &Json, _: ToolCall) -> ToolResult {
            ToolResult::ok("")
        }
        fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>> {
            Ok(config)
        }
        fn migrate(&self, config: Json, _: u32) -> Result<Json, Vec<ConfigError>> {
            Ok(config)
        }
        fn on_changed(&self, _: &Json, _: &Json) -> Vec<Message> {
            Vec::new()
        }
        fn on_removed(&self, _: &Json) -> Vec<Message> {
            Vec::new()
        }
    }

    fn backend(session: Uuid) -> MountBackend {
        let manifest: Manifest = serde_json::from_value(json!({
            "id": "notion", "name": "notion", "version": "1.0.0", "description": "",
            "tools": [],
            "config": { "version": 1, "schema": { "type": "object" }, "default": {} }
        }))
        .unwrap();
        let mut registry = Registry::new();
        registry
            .register(Arc::new(Mounter(manifest)), false)
            .unwrap();
        let routes = ProviderRoutes::new(
            &registry,
            &[Binding {
                plugin: "notion".into(),
                version: "1.0.0".into(),
                config_version: 1,
                config: json!({}),
                enabled: true,
                order: 3,
            }],
        );
        MountBackend::new(
            session,
            routes,
            Notices::new(),
            vec![
                Declaration {
                    plugin_id: "notion".into(),
                    name: "docs".into(),
                    read_only: true,
                },
                Declaration {
                    plugin_id: "notion".into(),
                    name: "notes".into(),
                    read_only: false,
                },
                Declaration {
                    plugin_id: "gone".into(),
                    name: "old".into(),
                    read_only: false,
                },
            ],
        )
    }

    /// Through the same path a live session uses: declarations become
    /// [`OneMount`]s handed to the scratch runtime, and the virtual shell
    /// reads the provider's files under `/mnt`.
    #[tokio::test]
    async fn a_session_serves_its_declarations_under_mnt() {
        use super::super::places::Unavailable;
        use super::super::runtime::{Call, Sandboxed, ScratchKey, ScratchRuntime};

        let dir = std::env::temp_dir().join(format!(
            "faber-api-mounts-{}-{}",
            std::process::id(),
            rand::random::<u32>()
        ));
        for sub in ["rootfs/etc", "rootfs/root", "rootfs/mnt", "rootfs/tmp"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let scratch = Sandboxed::with_config(scratch::ScratchConfig::new(
            dir.join("scratch"),
            dir.join("rootfs"),
        ));
        let session = Uuid::now_v7();
        let key = ScratchKey {
            session: session.to_string(),
            owner: Uuid::now_v7(),
        };
        let all = Arc::new(backend(session));
        let live = |declarations: Vec<Declaration>| -> Vec<LiveMount> {
            declarations
                .into_iter()
                .map(|d| LiveMount {
                    backend: Arc::new(OneMount {
                        all: Arc::clone(&all),
                        plugin: d.plugin_id.clone(),
                        mount: d.name.clone(),
                    }),
                    plugin: d.plugin_id,
                    name: d.name,
                    read_only: d.read_only,
                })
                .collect()
        };
        let run = async |command: &str| -> String {
            let routed: Result<_, Unavailable> = scratch
                .route(
                    &key,
                    Call::Exec {
                        command,
                        compute: false,
                    },
                )
                .await;
            let routed = routed.unwrap();
            let exit = routed
                .bound
                .target
                .exec(environment::Exec::new(command))
                .await
                .unwrap();
            let blobs = Arc::clone(&routed.bound.blobs);
            let text = |span: &environment::Span| {
                String::from_utf8(environment::Blobs::get(blobs.as_ref(), &span.blob).unwrap())
                    .unwrap()
            };
            format!("{}{}", text(&exit.stdout.span), text(&exit.stderr.span))
        };

        // Opening the session first, as the first call of a session does.
        run("true").await;
        scratch
            .set_mounts(&key.session, live(all.mounts_for_test()))
            .await;
        assert_eq!(run("ls /mnt/notion").await, "docs\nnotes\n");
        assert_eq!(
            run("cat /mnt/notion/docs/readme.md").await,
            "docs/readme.md"
        );
        assert!(
            run("echo x > /mnt/notion/docs/new.md")
                .await
                .contains("Read-only"),
            "a read-only mount refuses a write"
        );

        // Unmounted: gone from the session at once.
        scratch.set_mounts(&key.session, Vec::new()).await;
        assert!(!run("ls /mnt").await.contains("notion"));

        scratch.delete(&key.session).await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn calls_reach_the_owner_and_read_only_is_enforced_first() {
        let session = Uuid::now_v7();
        let backend = backend(session);

        let entry = backend.stat("notion", "docs", "a//b/").await.unwrap();
        assert_eq!(entry.name, format!("notion:{session}:docs:/a/b"));
        assert_eq!(
            backend.read("notion", "docs", "/x", 0, 10).await.unwrap(),
            b"docs/x"
        );

        assert_eq!(
            backend
                .put("notion", "docs", "/x", b"hi".to_vec(), None, false)
                .await,
            Err(FsError::ReadOnly)
        );
        assert_eq!(
            backend.remove("notion", "docs", "/x", false).await,
            Err(FsError::ReadOnly)
        );
        assert_eq!(
            backend
                .put("notion", "notes", "/x", b"hi".to_vec(), None, false)
                .await
                .unwrap()
                .size,
            2
        );
        assert_eq!(
            backend.remove("notion", "notes", "/", true).await,
            Err(FsError::Denied)
        );
        assert_eq!(
            backend.stat("notion", "notes", "/..").await,
            Err(FsError::Denied)
        );

        // Not declared, or declared by a plugin that no longer serves.
        assert_eq!(
            backend.stat("notion", "elsewhere", "/").await,
            Err(FsError::NotFound)
        );
        assert_eq!(
            backend.stat("gone", "old", "/").await,
            Err(FsError::NotFound)
        );

        backend.set_mounts(Vec::new());
        assert_eq!(
            backend.stat("notion", "docs", "/").await,
            Err(FsError::NotFound)
        );
    }

    // --- The store, against the database DATABASE_URL names (ignored tests). ---

    struct Fixture {
        pool: DbPool,
        user: Uuid,
        project: Uuid,
        sessions: Vec<Uuid>,
    }

    impl Fixture {
        async fn new(sessions: usize) -> Option<Fixture> {
            let url = std::env::var("DATABASE_URL").expect("set DATABASE_URL (see .env)");
            let pool = crate::db::init_pool(&url).await;
            let mut conn = pool.get().await.unwrap();
            let user = Uuid::now_v7();
            diesel::sql_query("INSERT INTO users (id, identity_id) VALUES ($1, $2)")
                .bind::<diesel::sql_types::Uuid, _>(user)
                .bind::<diesel::sql_types::Uuid, _>(Uuid::now_v7())
                .execute(&mut conn)
                .await
                .unwrap();
            let workspace = crate::access::personal_workspace(&mut conn, user)
                .await
                .unwrap();
            let project = Uuid::now_v7();
            diesel::sql_query("INSERT INTO project (id, owner_id, name) VALUES ($1, $2, $3)")
                .bind::<diesel::sql_types::Uuid, _>(project)
                .bind::<diesel::sql_types::Uuid, _>(user)
                .bind::<diesel::sql_types::Text, _>(format!("mounts-{project}"))
                .execute(&mut conn)
                .await
                .unwrap();
            let mut ids = Vec::new();
            for _ in 0..sessions {
                let id = Uuid::now_v7();
                diesel::insert_into(crate::schema::session::table)
                    .values(crate::models::session::NewSession {
                        id,
                        workspace_id: workspace.id,
                        title: None,
                        created_at: crate::models::now_epoch(),
                        project_id: Some(project),
                    })
                    .execute(&mut conn)
                    .await
                    .unwrap();
                ids.push(id);
            }
            drop(conn);
            Some(Fixture {
                pool,
                user,
                project,
                sessions: ids,
            })
        }

        async fn cleanup(self) {
            let mut conn = self.pool.get().await.unwrap();
            for statement in [
                "DELETE FROM session WHERE project_id = $1",
                "DELETE FROM project WHERE id = $1",
            ] {
                diesel::sql_query(statement)
                    .bind::<diesel::sql_types::Uuid, _>(self.project)
                    .execute(&mut conn)
                    .await
                    .unwrap();
            }
            let _ = diesel::sql_query("DELETE FROM users WHERE id = $1")
                .bind::<diesel::sql_types::Uuid, _>(self.user)
                .execute(&mut conn)
                .await;
        }
    }

    async fn bind(fixture: &Fixture, plugin: &str, enabled: bool) {
        let mut conn = fixture.pool.get().await.unwrap();
        diesel::sql_query(
            "INSERT INTO project_binding (project_id, plugin_type, version, config_version, config, enabled) \
             VALUES ($1, $2, '1.0.0', 1, '{}', $3)",
        )
        .bind::<diesel::sql_types::Uuid, _>(fixture.project)
        .bind::<diesel::sql_types::Text, _>(plugin)
        .bind::<diesel::sql_types::Bool, _>(enabled)
        .execute(&mut conn)
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn a_plugin_turned_off_keeps_its_mounts_unserved_and_a_removed_one_loses_them() {
        let Some(fixture) = Fixture::new(1).await else {
            return;
        };
        let session = fixture.sessions[0];
        bind(&fixture, "drive", true).await;
        bind(&fixture, "notion", false).await;
        let places = Arc::new(Places::new(
            fixture.pool.clone(),
            Arc::default(),
            Arc::new(super::super::runtime::Sandboxed::off()),
        ));
        let mounts = SessionMounts::new(fixture.pool.clone(), places);
        for (plugin, name) in [("drive", "files"), ("notion", "docs"), ("gone", "old")] {
            mounts
                .service()
                .mount(session, plugin, Scratch::Enabled, &spec(name, false))
                .await
                .unwrap();
        }

        // Only what a plugin turned on declared is served, and listed.
        let served: Vec<String> = mounts
            .served(session)
            .await
            .iter()
            .map(Declaration::path)
            .collect();
        assert_eq!(served, ["/mnt/drive/files"]);

        // Restoring drops only the plugin no longer in the project; the one
        // turned off keeps its declaration for when it is turned on again.
        let gone = mounts.restore(session).await.expect("a gone notice");
        assert!(
            plugin::joined(&gone.content).contains("/mnt/gone/old"),
            "{gone:?}"
        );
        let kept: Vec<String> = mounts
            .service()
            .list(session)
            .await
            .unwrap()
            .iter()
            .map(Declaration::path)
            .collect();
        assert_eq!(kept, ["/mnt/drive/files", "/mnt/notion/docs"]);
        fixture.cleanup().await;
    }

    fn spec(name: &str, read_only: bool) -> MountSpec {
        MountSpec {
            name: name.into(),
            read_only,
        }
    }

    fn refused<T: std::fmt::Debug>(result: Result<T, MountFailure>) -> MountError {
        match result {
            Err(MountFailure::Refused(error)) => error,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn mounts_are_stored_replaced_capped_and_removed() {
        let Some(fixture) = Fixture::new(1).await else {
            return;
        };
        let service = MountService::with_cap(fixture.pool.clone(), 2);
        let session = fixture.sessions[0];

        assert_eq!(
            refused(
                service
                    .mount(session, "notion", Scratch::Missing, &spec("docs", true))
                    .await
            ),
            MountError::NoScratch
        );
        assert_eq!(
            refused(
                service
                    .mount(session, "notion", Scratch::Disabled, &spec("docs", true))
                    .await
            ),
            MountError::ScratchDisabled
        );
        assert_eq!(
            refused(
                service
                    .mount(session, "notion", Scratch::Enabled, &spec("../x", true))
                    .await
            ),
            MountError::InvalidName
        );

        let first = service
            .mount(session, "notion", Scratch::Enabled, &spec("docs", true))
            .await
            .unwrap();
        assert_eq!(first.declaration.path(), "/mnt/notion/docs");
        assert_eq!(first.replaced, None);
        service
            .mount(session, "drive", Scratch::Enabled, &spec("docs", false))
            .await
            .unwrap();

        // At the cap a new name is refused, but remounting an existing one
        // replaces it in place.
        assert_eq!(
            refused(
                service
                    .mount(session, "notion", Scratch::Enabled, &spec("more", false))
                    .await
            ),
            MountError::TooMany
        );
        let again = service
            .mount(session, "notion", Scratch::Enabled, &spec("docs", false))
            .await
            .unwrap();
        assert_eq!(again.replaced, Some(first.declaration.clone()));
        let listed = service.list(session).await.unwrap();
        assert_eq!(
            listed.iter().map(Declaration::path).collect::<Vec<_>>(),
            ["/mnt/notion/docs", "/mnt/drive/docs"]
        );
        assert!(!listed[0].read_only);

        // A plugin only ever unmounts its own.
        assert_eq!(
            refused(service.unmount(session, "drive", "nope").await),
            MountError::NotFound
        );
        assert_eq!(
            refused(service.unmount(session, "drive", "").await),
            MountError::InvalidName
        );
        let removed = service.unmount(session, "drive", "docs").await.unwrap();
        assert_eq!(removed.plugin_id, "drive");
        assert_eq!(service.list(session).await.unwrap().len(), 1);

        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn concurrent_mounts_in_one_session_respect_the_cap() {
        let Some(fixture) = Fixture::new(1).await else {
            return;
        };
        let service = Arc::new(MountService::with_cap(fixture.pool.clone(), 3));
        let session = fixture.sessions[0];
        let tasks: Vec<_> = (0..8)
            .map(|n| {
                let service = Arc::clone(&service);
                tokio::spawn(async move {
                    service
                        .mount(
                            session,
                            "notion",
                            Scratch::Enabled,
                            &spec(&format!("m{n}"), false),
                        )
                        .await
                })
            })
            .collect();
        let mut ok = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => ok += 1,
                Err(MountFailure::Refused(MountError::TooMany)) => {}
                Err(other) => panic!("{other}"),
            }
        }
        assert_eq!(ok, 3);
        assert_eq!(service.list(session).await.unwrap().len(), 3);
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn restore_and_removal_drop_a_gone_plugins_mounts() {
        let Some(fixture) = Fixture::new(2).await else {
            return;
        };
        let service = MountService::new(fixture.pool.clone());
        let (one, two) = (fixture.sessions[0], fixture.sessions[1]);
        for (session, plugin, name) in [
            (one, "notion", "docs"),
            (one, "drive", "files"),
            (two, "notion", "a"),
            (two, "notion", "b"),
        ] {
            service
                .mount(session, plugin, Scratch::Enabled, &spec(name, false))
                .await
                .unwrap();
        }

        let restored = service.restore(one, &["drive".into()]).await.unwrap();
        assert_eq!(
            restored
                .kept
                .iter()
                .map(Declaration::path)
                .collect::<Vec<_>>(),
            ["/mnt/drive/files"]
        );
        assert_eq!(restored.gone, ["/mnt/notion/docs"]);
        assert_eq!(service.list(one).await.unwrap().len(), 1);

        let mut conn = fixture.pool.get().await.unwrap();
        let dropped = drop_plugin(&mut conn, fixture.project, "notion")
            .await
            .unwrap();
        drop(conn);
        assert_eq!(
            dropped,
            [(
                two,
                vec!["/mnt/notion/a".to_owned(), "/mnt/notion/b".to_owned()]
            )]
        );
        assert!(service.list(two).await.unwrap().is_empty());
        assert_eq!(service.list(one).await.unwrap().len(), 1);

        fixture.cleanup().await;
    }

    struct Context {
        scratch: Scratch,
        synced: Mutex<Vec<Uuid>>,
        told: Mutex<Vec<(Uuid, String)>>,
    }

    #[async_trait]
    impl MountsContext for Context {
        async fn scratch(&self, _: Uuid) -> Result<Scratch, String> {
            Ok(self.scratch)
        }
        async fn sync(&self, session: Uuid) {
            self.synced.lock().unwrap().push(session);
        }
        async fn notify(&self, session: Uuid, notice: Message) {
            self.told.lock().unwrap().push((session, notice.text()));
        }
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn the_host_interface_mounts_in_the_callers_name() {
        let Some(fixture) = Fixture::new(1).await else {
            return;
        };
        let session = fixture.sessions[0];
        let context = Arc::new(Context {
            scratch: Scratch::Enabled,
            synced: Mutex::new(Vec::new()),
            told: Mutex::new(Vec::new()),
        });
        let host = MountsHost::new(
            Arc::new(MountService::new(fixture.pool.clone())),
            context.clone(),
        );
        let ctx = SessionCtx::new(session.to_string(), "notion", 2, Notices::new());

        let path = host.mount(&ctx, spec("docs", true)).await.unwrap();
        assert_eq!(path, "/mnt/notion/docs");
        // The same mount again changes nothing, so nothing is said.
        host.mount(&ctx, spec("docs", true)).await.unwrap();
        host.unmount(&ctx, "docs").await.unwrap();
        assert_eq!(host.unmount(&ctx, "docs").await, Err(MountError::NotFound));

        let told = context.told.lock().unwrap().clone();
        assert_eq!(told.len(), 2, "{told:?}");
        assert!(told[0].1.contains("is now mounted"));
        assert!(told[1].1.contains("unmounted"));
        // The live scratch is brought in step with each change, before the
        // agent hears of it.
        assert_eq!(*context.synced.lock().unwrap(), vec![session, session]);
        fixture.cleanup().await;
    }
}
