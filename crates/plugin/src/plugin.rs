//! `interface host` and `interface plugin`, for built-ins.
//!
//! A built-in implements [`Plugin`] natively — the same exports a component
//! has, with the same rules: stateless by contract, every export given what it
//! needs as arguments, and module state only ever a cache.

use std::sync::Arc;

use async_trait::async_trait;

use crate::manifest::Manifest;
use crate::notice::{Notices, Pending};
use crate::types::{
    ConfigError, Content, Delivery, Json, Message, Phase, PostError, ToolCall, ToolResult,
};

/// One session snapshot's context, as one binding sees it.
///
/// A component only ever borrows it for a call; a built-in may clone it, and
/// may keep the handles `post` returns as long as it likes.
#[derive(Clone, Debug)]
pub struct SessionCtx {
    session_id: String,
    binding: String,
    order: i64,
    notices: Arc<Notices>,
}

impl SessionCtx {
    pub fn new(
        session_id: impl Into<String>,
        binding: impl Into<String>,
        order: i64,
        notices: Arc<Notices>,
    ) -> Self {
        SessionCtx {
            session_id: session_id.into(),
            binding: binding.into(),
            order,
            notices,
        }
    }

    /// Opaque and stable for the session's life. Key session-specific cache
    /// entries by it.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn post(&self, notice: Message, delivery: Delivery) -> Result<Pending, PostError> {
        self.notices
            .post(&self.binding, self.order, notice, delivery)
    }
}

/// What a plugin type implements.
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Static data, read without calling anything else.
    fn manifest(&self) -> &Manifest;

    /// Interfaces this plugin exports to other bindings
    /// (`faber:environments/handle`). In v1 only built-ins export.
    fn exports(&self) -> Vec<String> {
        Vec::new()
    }

    /// Interfaces this plugin imports from other bindings. For a component
    /// these are read from its imports; a built-in names them.
    fn imports(&self) -> Vec<String> {
        Vec::new()
    }

    /// Once per binding per session snapshot. Returns `system`: head text,
    /// used only when `phase` is `fresh`, 4 KiB at most. An error goes to logs
    /// and the UI, never to the model, and degrades the binding.
    async fn open(
        &self,
        ctx: &SessionCtx,
        phase: Phase,
        config: &Json,
    ) -> Result<Vec<Content>, String>;

    /// May run concurrently with other calls. Never cancelled; unknown tools
    /// and schema failures never reach it.
    async fn handle_tool(&self, ctx: &SessionCtx, config: &Json, call: ToolCall) -> ToolResult;

    /// Pure. Returns the normalized config.
    fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>>;

    /// Pure. Brings a config written under config version `from` to the
    /// manifest's.
    fn migrate(&self, config: Json, from: u32) -> Result<Json, Vec<ConfigError>>;

    /// Pure. What the agent is told about a config change or upgrade; empty
    /// is silence.
    fn on_changed(&self, old: &Json, new: &Json) -> Vec<Message>;

    /// Pure. What the agent is told when the binding is removed or disabled.
    fn on_removed(&self, config: &Json) -> Vec<Message>;
}
