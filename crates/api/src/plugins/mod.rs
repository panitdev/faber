//! The plugins this server hosts, and the per-session state the core keeps
//! for them.
//!
//! Every plugin here is a built-in: Rust implementing `faber:plugin@1.0.0`
//! natively ([`plugin::Plugin`]). Environments is bound to new projects by
//! default, and so is web search when the server has an engine.

pub mod environments;
pub mod web;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use plugin::{Notices, Registry};
use uuid::Uuid;

use crate::agent::AgentRegistry;
use crate::db::DbPool;

/// The core prompt: the first part of every project session's system head,
/// before each binding's own `system`.
pub const CORE_PROMPT: &str = "You are Panit, an agent that does work for the user through tools. Act with tools when a call answers the question; be direct and concise, and report what you did.";

pub struct Plugins {
    pub registry: Arc<Registry>,
    /// Each session's notice queue. In-process, like the run registry: a
    /// notice reaches the next run on the instance that holds it.
    notices: Mutex<HashMap<Uuid, Arc<Notices>>>,
    environments: Arc<environments::places::Places>,
    plugin: Arc<environments::Environments>,
    mounts: Arc<environments::mounts::SessionMounts>,
    db: DbPool,
    /// The host side of `faber:environments/mounts`, for a built-in that
    /// imports it: such a plugin takes it at construction and names
    /// [`environments::MOUNTS`] among its imports.
    // No built-in mounts yet; this is where one gets its host.
    #[allow(dead_code)]
    pub mounts_host: Arc<dyn plugin::environments::Mounts>,
}

impl Plugins {
    pub fn new(
        db: DbPool,
        agents: Arc<AgentRegistry>,
        scratch: &crate::config::ScratchSettings,
        search: Option<Arc<dyn search::SearchEngine>>,
    ) -> Self {
        Plugins::with_scratch(
            db,
            agents,
            Arc::new(environments::runtime::Sandboxed::new(scratch)),
            search,
        )
    }

    pub fn with_scratch(
        db: DbPool,
        agents: Arc<AgentRegistry>,
        scratch: Arc<dyn environments::runtime::ScratchRuntime>,
        search: Option<Arc<dyn search::SearchEngine>>,
    ) -> Self {
        let places = Arc::new(environments::places::Places::new(
            db.clone(),
            agents,
            scratch,
        ));
        let mounts = Arc::new(environments::mounts::SessionMounts::new(
            db.clone(),
            Arc::clone(&places),
        ));
        let mounts_host: Arc<dyn plugin::environments::Mounts> =
            Arc::new(environments::mounts::MountsHost::new(
                Arc::clone(mounts.service()),
                Arc::clone(&mounts) as Arc<dyn environments::mounts::MountsContext>,
            ));

        let plugin = Arc::new(environments::Environments::new(
            db.clone(),
            Arc::clone(&places),
            Arc::clone(&mounts),
        ));
        let mut registry = Registry::new();
        registry
            .register(Arc::clone(&plugin) as Arc<dyn plugin::Plugin>, true)
            .expect("the environments plugin's manifest is valid");
        if let Some(engine) = search {
            registry
                .register(Arc::new(web::Web::new(engine)), true)
                .expect("the web plugin's manifest is valid");
        }

        Plugins {
            registry: Arc::new(registry),
            notices: Mutex::new(HashMap::new()),
            environments: places,
            plugin,
            mounts,
            db,
            mounts_host,
        }
    }

    /// A session's notice queue, made on first use.
    pub fn notices(&self, session: Uuid) -> Arc<Notices> {
        Arc::clone(
            self.notices
                .lock()
                .expect("notice registry poisoned")
                .entry(session)
                .or_default(),
        )
    }

    /// A session's notice queue, only when this process already keeps one.
    pub fn existing_notices(&self, session: Uuid) -> Option<Arc<Notices>> {
        self.notices
            .lock()
            .expect("notice registry poisoned")
            .get(&session)
            .cloned()
    }

    /// Hands the environments plugin the state user environments are bound
    /// with. Called once, right after that state is built.
    pub fn attach(&self, state: crate::state::AppState) {
        self.environments.attach(state);
    }

    /// Archives and stops sandboxes an earlier process of this server left
    /// running, and deletes the scratches of sessions that are gone — a
    /// process that died between deleting a session and its scratch leaves
    /// one behind. Called once at startup, before any request is served.
    pub async fn recover(&self) {
        let scratch = self.environments.scratch();
        scratch.recover().await;
        let stored: Vec<Uuid> = scratch
            .stored_sessions()
            .await
            .iter()
            .filter_map(|id| id.parse().ok())
            .collect();
        if stored.is_empty() {
            return;
        }
        let live: Vec<Uuid> = match self.db.get().await {
            Ok(mut conn) => {
                use diesel::{ExpressionMethods, QueryDsl};
                use diesel_async::RunQueryDsl;
                match crate::schema::session::table
                    .filter(crate::schema::session::id.eq_any(&stored))
                    .select(crate::schema::session::id)
                    .load(&mut conn)
                    .await
                {
                    Ok(live) => live,
                    Err(error) => {
                        tracing::warn!(%error, "scratches of deleted sessions not swept");
                        return;
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "scratches of deleted sessions not swept");
                return;
            }
        };
        for session in stored.into_iter().filter(|id| !live.contains(id)) {
            tracing::info!(%session, "deleting the scratch of a deleted session");
            scratch.delete(&session.to_string()).await;
        }
    }

    /// Deletes a session's scratch. Its mount declarations go with the
    /// session row, by cascade.
    pub async fn session_deleted(&self, session: Uuid) {
        self.plugin.forget(&session.to_string()).await;
        self.environments
            .scratch()
            .delete(&session.to_string())
            .await;
        self.notices
            .lock()
            .expect("notice registry poisoned")
            .remove(&session);
    }

    /// A plugin was removed from a project, and `lost` is what
    /// [`environments::mounts::drop_plugin`] dropped in that transaction:
    /// each session serves what is left and is told.
    pub async fn plugin_removed(&self, lost: Vec<(Uuid, Vec<String>)>) {
        self.mounts.plugin_removed(lost).await;
    }

    /// A plugin of `project` was turned on or off: its mounts stay declared
    /// and are served only while it is on.
    pub async fn plugin_toggled(&self, project: Uuid) {
        self.mounts.plugin_toggled(project).await;
    }

    /// Asks the scratch runtime to stop what has been idle past its timeout,
    /// every minute, for the life of the process.
    pub fn spawn_reaper(self: &Arc<Self>) {
        let places = Arc::clone(&self.environments);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(60));
            loop {
                every.tick().await;
                places.scratch().reap_idle().await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn startup_deletes_the_scratch_of_a_session_that_is_gone() {
        use diesel_async::RunQueryDsl;

        let url = std::env::var("DATABASE_URL").expect("set DATABASE_URL (see .env)");
        let db = crate::db::init_pool(&url).await;
        let dir = std::env::temp_dir().join(format!("faber-sweep-{}", Uuid::now_v7()));
        let rootfs = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let scratch = Arc::new(environments::runtime::Sandboxed::with_config(
            scratch::ScratchConfig::new(dir.join("scratch"), &rootfs),
        ));

        // A live session, and a scratch whose session row is gone.
        let mut conn = db.get().await.unwrap();
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
        let live = Uuid::now_v7();
        diesel::insert_into(crate::schema::session::table)
            .values(crate::models::session::NewSession {
                id: live,
                workspace_id: workspace.id,
                title: None,
                created_at: crate::models::now_epoch(),
                project_id: None,
            })
            .execute(&mut conn)
            .await
            .unwrap();
        drop(conn);
        let gone = Uuid::now_v7();
        for session in [live, gone] {
            let at = dir.join("scratch/sessions").join(session.to_string());
            std::fs::create_dir_all(at.join("layer")).unwrap();
            std::fs::write(at.join("owner"), user.to_string()).unwrap();
        }

        let plugins = Plugins::with_scratch(db.clone(), Arc::default(), scratch, None);
        plugins.recover().await;
        let sessions = dir.join("scratch/sessions");
        assert!(sessions.join(live.to_string()).exists());
        assert!(!sessions.join(gone.to_string()).exists());

        let mut conn = db.get().await.unwrap();
        for statement in [
            "DELETE FROM session WHERE id = $1",
            "DELETE FROM users WHERE id = $2",
        ] {
            diesel::sql_query(statement)
                .bind::<diesel::sql_types::Uuid, _>(live)
                .bind::<diesel::sql_types::Uuid, _>(user)
                .execute(&mut conn)
                .await
                .unwrap();
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
