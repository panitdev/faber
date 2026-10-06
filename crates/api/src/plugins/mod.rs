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
}

impl Plugins {
    pub fn new(
        db: DbPool,
        agents: Arc<AgentRegistry>,
        scratch_root: Option<std::path::PathBuf>,
        search: Option<Arc<dyn search::SearchEngine>>,
    ) -> Self {
        if let Some(root) = &scratch_root {
            tracing::warn!(
                root = %root.display(),
                "scratch is enabled: project sandboxes run unsandboxed on this server"
            );
        }
        let places = Arc::new(environments::places::Places::new(
            db.clone(),
            agents,
            scratch_root.map(environments::places::Scratch::new),
        ));

        let mut registry = Registry::new();
        registry
            .register(
                Arc::new(environments::Environments::new(db, Arc::clone(&places))),
                true,
            )
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
        }
    }

    /// A session's notice queue, made on first use.
    pub fn notices(&self, session: Uuid) -> Arc<Notices> {
        Arc::clone(
            self.notices
                .lock()
                .expect("notice registry poisoned")
                .entry(session)
                .or_insert_with(Notices::new),
        )
    }

    /// Evicts idle scratch every hour, for the life of the process.
    pub fn spawn_reaper(self: &Arc<Self>) {
        let places = Arc::clone(&self.environments);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(60 * 60));
            loop {
                every.tick().await;
                places.evict_idle_scratch().await;
            }
        });
    }
}
