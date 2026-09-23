use axum::extract::FromRef;
use std::sync::Arc;

use crate::{
    agent::AgentRegistry,
    config::Config,
    db::DbPool,
    presentation::resolve::ContainerAddressCache,
    run::{InterruptRegistry, RunRegistry},
    ssh_pool::SshSessionPool,
};

/// 32-byte master key for envelope encryption. Never printed — Debug is intentionally redacted.
#[derive(Clone)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub config: Config,
    pub http: reqwest::Client,
    /// Surge session/identity provider (remote — talks to `surge-server` over HTTP).
    pub auth: Arc<dyn surge::AuthProvider>,
    /// Envelope encryption master key. Loaded at boot; absent key panics before serving traffic.
    pub master_key: Arc<MasterKey>,
    /// The web search engine every run shares, when one is configured. Built
    /// once at boot (`crate::search`) rather than per run: the public provider
    /// discovers and probes instances, and that belongs to the process, not to
    /// a user's turn. `None` is a service where no run is granted `search`.
    pub search: Option<Arc<dyn search::SearchEngine>>,
    /// Live harness runs, keyed by session — what the SSE endpoint subscribes
    /// to. In-process, so it only reaches subscribers on the instance that
    /// owns the run.
    pub runs: RunRegistry,
    /// Live harness runs that can still be stopped, keyed by run. In-process
    /// for the same reason `runs` is, and with the same limit: an interrupt
    /// only reaches a run on the instance that owns it.
    pub interrupts: InterruptRegistry,
    /// Connected agent-transport daemons, keyed by host. In-process for the
    /// same reason `runs` is — a bind only finds a daemon whose connection
    /// landed on this process, and faber runs as one.
    pub agents: Arc<AgentRegistry>,
    /// Authenticated SSH sessions shared across binds and preview channels.
    pub ssh: Arc<SshSessionPool>,
    pub presentation_addresses: Arc<ContainerAddressCache>,
}

/// Required by `surge::AuthSession`, which resolves the provider off the router state.
impl AsRef<Arc<dyn surge::AuthProvider>> for AppState {
    fn as_ref(&self) -> &Arc<dyn surge::AuthProvider> {
        &self.auth
    }
}

impl FromRef<AppState> for DbPool {
    fn from_ref(s: &AppState) -> Self {
        s.db.clone()
    }
}

impl FromRef<AppState> for Config {
    fn from_ref(s: &AppState) -> Self {
        s.config.clone()
    }
}
