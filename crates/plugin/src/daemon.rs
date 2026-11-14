//! Plugin daemons and per-binding storage, for built-ins.
//!
//! A draft extension of `faber:plugin@1.0.0` that leaves it unchanged: a
//! plugin may run one long-lived [`Daemon`] that serves every enabled binding
//! of its version, reaches each project only through a host-owned
//! [`BindingHandle`], and keeps what must survive a restart in that binding's
//! private [`Store`]. Tool handlers reach the same store, and the key their
//! session was opened under, through [`DaemonHost`].
//!
//! These traits mirror the draft `faber:daemon@1.0.0` WIT and will be frozen
//! into it once a built-in (Discord) has exercised them. Until third-party
//! components are hosted, a built-in implements [`Daemon`] natively and hands
//! it out from [`crate::Plugin::daemon`].
//!
//! Nothing here is global. The host derives the project from the handle, or
//! from the calling session, and never from an argument, so no plugin can name
//! another binding's sessions or store.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::plugin::SessionCtx;
use crate::types::{Json, Message, PostError};

// --- limits ---
//
// The contract's proposed values. The operator may set others; these are
// what the host uses when nothing else is configured.

/// Messages that may wait on one session before `send` answers `busy`.
pub const PENDING_CAP: usize = 16;
/// How long a dedupe id is remembered against its session.
pub const DEDUPE_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// Bytes of one stored value.
pub const RECORD_CAP: usize = 256 * 1024;
/// Bytes one binding's store may hold, keys and values together.
pub const BINDING_CAP: u64 = 16 * 1024 * 1024;
/// Bytes of a session key or a storage key.
pub const KEY_CAP: usize = 128;
/// Bytes of a dedupe id.
pub const DEDUPE_CAP: usize = 128;
/// Bytes of a session title.
pub const TITLE_CAP: usize = 256;
/// Most keys one `list` call returns, whatever `limit` asks for.
pub const LIST_CAP: u32 = 1000;
/// Time `serve` gets to return once its event stream has closed.
pub const GRACE: Duration = Duration::from_secs(10);

// --- types ---

/// `record session-ref`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    /// The same opaque id [`SessionCtx::session_id`] returns.
    pub id: String,
    /// `false` when the key already named a live session.
    pub created: bool,
}

/// `enum session-error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "kebab-case")]
pub enum SessionError {
    /// The handle was replaced by a config change, or the binding removed.
    #[error("gone")]
    Gone,
    /// The session was not opened by this binding.
    #[error("not-owned")]
    NotOwned,
    #[error("not-found")]
    NotFound,
    /// The project or the operator refuses more sessions right now.
    #[error("limit")]
    Limit,
    /// A key is 1 to [`KEY_CAP`] bytes of printable ASCII.
    #[error("invalid-key")]
    InvalidKey,
}

/// `enum send-error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "kebab-case")]
pub enum SendError {
    #[error("gone")]
    Gone,
    #[error("not-owned")]
    NotOwned,
    #[error("not-found")]
    NotFound,
    #[error("archived")]
    Archived,
    /// [`PENDING_CAP`] messages already wait on this session.
    #[error("busy")]
    Busy,
    /// Only `user` messages are accepted.
    #[error("role-refused")]
    RoleRefused,
    #[error("too-large")]
    TooLarge,
    #[error("empty")]
    Empty,
}

/// `record entry`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    pub value: Vec<u8>,
    /// Changes on every write; never 0, which `put` reserves for
    /// create-only.
    pub version: u64,
}

/// `enum storage-error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "kebab-case")]
pub enum StorageError {
    #[error("gone")]
    Gone,
    #[error("not-found")]
    NotFound,
    /// `if-version` no longer matches.
    #[error("conflict")]
    Conflict,
    /// The write would pass the binding's cap or the project's quota.
    /// Nothing is evicted to make room.
    #[error("quota")]
    Quota,
    /// The value is over [`RECORD_CAP`].
    #[error("too-large")]
    TooLarge,
    #[error("invalid-key")]
    InvalidKey,
}

// --- checks the host applies ---

/// Session keys and storage keys: 1 to [`KEY_CAP`] bytes of printable ASCII
/// (space through `~`).
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= KEY_CAP && key.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// What `send` refuses before anything is written: every message a notice
/// would refuse — empty, or over the v1 message size — and anything but a
/// `user` message, since the daemon speaks for a channel's people. A dedupe
/// id over [`DEDUPE_CAP`] bytes or outside printable ASCII is refused as
/// `too-large`; the contract has no case of its own for it.
pub fn check_send(message: &Message, dedupe: Option<&str>) -> Result<(), SendError> {
    if message.role != crate::types::Role::User {
        return Err(SendError::RoleRefused);
    }
    crate::notice::check(message).map_err(|error| match error {
        PostError::RoleRefused => SendError::RoleRefused,
        PostError::TooLarge => SendError::TooLarge,
        PostError::Empty | PostError::QueueFull => SendError::Empty,
    })?;
    if let Some(dedupe) = dedupe
        && (dedupe.is_empty()
            || dedupe.len() > DEDUPE_CAP
            || !dedupe.bytes().all(|b| (0x20..=0x7e).contains(&b)))
    {
        return Err(SendError::TooLarge);
    }
    Ok(())
}

// --- storage ---

/// `interface storage`, `resource store`: one binding's private key-value
/// store. Keys are strings, values bytes, and every record carries a version
/// for compare-and-set.
#[async_trait]
pub trait Store: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Result<Entry, StorageError>;

    /// Writes `value` and returns its new version. `if_version`: `None`
    /// writes unconditionally, `Some(0)` creates only, `Some(v)` replaces
    /// only the record at version `v`; a mismatch is `conflict`.
    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
        if_version: Option<u64>,
    ) -> Result<u64, StorageError>;

    /// `not-found` when there is no record; `conflict` when `if_version` is
    /// given and does not match.
    async fn delete(&self, key: &str, if_version: Option<u64>) -> Result<(), StorageError>;

    /// Keys under `prefix`, in key order, after `after` when given, at most
    /// `limit` (and never more than [`LIST_CAP`]). Pass the last key back as
    /// `after` to page.
    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<String>, StorageError>;

    /// Bytes used and bytes allowed: the smaller of what the binding's cap and
    /// the project's quota leave this store.
    async fn usage(&self) -> Result<(u64, u64), StorageError>;
}

pub type SharedStore = Arc<dyn Store>;

/// What a record costs against a cap: its key and its value.
pub fn record_size(key: &str, value: &[u8]) -> u64 {
    (key.len() + value.len()) as u64
}

/// A store in memory, with the full semantics and the binding cap, but no
/// project quota and no persistence. For tests and fakes; the host's store is
/// in Postgres.
#[derive(Debug)]
pub struct MemoryStore {
    cap: u64,
    inner: Mutex<MemoryInner>,
}

#[derive(Debug, Default)]
struct MemoryInner {
    records: BTreeMap<String, (Vec<u8>, u64)>,
    next_version: u64,
    gone: bool,
}

impl MemoryStore {
    pub fn new(cap: u64) -> Arc<Self> {
        Arc::new(MemoryStore {
            cap,
            inner: Mutex::new(MemoryInner {
                next_version: 1,
                ..MemoryInner::default()
            }),
        })
    }

    /// Every later call answers `gone`, as a store of a removed binding does.
    pub fn close(&self) {
        self.inner.lock().unwrap().gone = true;
    }
}

impl MemoryInner {
    fn used(&self) -> u64 {
        self.records
            .iter()
            .map(|(key, (value, _))| record_size(key, value))
            .sum()
    }

    fn live(&self) -> Result<(), StorageError> {
        if self.gone {
            Err(StorageError::Gone)
        } else {
            Ok(())
        }
    }
}

fn matches(current: Option<u64>, wanted: Option<u64>) -> bool {
    match wanted {
        None => true,
        Some(0) => current.is_none(),
        Some(version) => current == Some(version),
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn get(&self, key: &str) -> Result<Entry, StorageError> {
        let inner = self.inner.lock().unwrap();
        inner.live()?;
        if !valid_key(key) {
            return Err(StorageError::InvalidKey);
        }
        inner
            .records
            .get(key)
            .map(|(value, version)| Entry {
                key: key.to_owned(),
                value: value.clone(),
                version: *version,
            })
            .ok_or(StorageError::NotFound)
    }

    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
        if_version: Option<u64>,
    ) -> Result<u64, StorageError> {
        let mut inner = self.inner.lock().unwrap();
        inner.live()?;
        if !valid_key(key) {
            return Err(StorageError::InvalidKey);
        }
        if value.len() > RECORD_CAP {
            return Err(StorageError::TooLarge);
        }
        let current = inner.records.get(key);
        if !matches(current.map(|(_, version)| *version), if_version) {
            return Err(StorageError::Conflict);
        }
        let freed = current.map_or(0, |(old, _)| record_size(key, old));
        if inner.used() - freed + record_size(key, &value) > self.cap {
            return Err(StorageError::Quota);
        }
        let version = inner.next_version;
        inner.next_version += 1;
        inner.records.insert(key.to_owned(), (value, version));
        Ok(version)
    }

    async fn delete(&self, key: &str, if_version: Option<u64>) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().unwrap();
        inner.live()?;
        if !valid_key(key) {
            return Err(StorageError::InvalidKey);
        }
        let Some((_, version)) = inner.records.get(key) else {
            return Err(StorageError::NotFound);
        };
        if if_version.is_some_and(|wanted| wanted != *version) {
            return Err(StorageError::Conflict);
        }
        inner.records.remove(key);
        Ok(())
    }

    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<String>, StorageError> {
        let inner = self.inner.lock().unwrap();
        inner.live()?;
        let limit = limit.min(LIST_CAP) as usize;
        Ok(inner
            .records
            .keys()
            .filter(|key| key.starts_with(prefix))
            .filter(|key| after.is_none_or(|after| key.as_str() > after))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn usage(&self) -> Result<(u64, u64), StorageError> {
        let inner = self.inner.lock().unwrap();
        inner.live()?;
        Ok((inner.used(), self.cap))
    }
}

// --- host ---

/// `interface host`, `resource binding`: one enabled binding of this plugin,
/// in one project. Host-owned; the daemon calls it and never implements it.
///
/// A handle answers `gone` from the moment a `changed` event replaces it or a
/// `removed` event retires its binding.
#[async_trait]
pub trait BindingHandle: Send + Sync + 'static {
    /// Opaque, stable across config changes and restarts.
    fn id(&self) -> &str;

    /// Normalized, at the manifest's config version: it has passed `migrate`
    /// and `validate` as at bind time.
    fn config(&self) -> &Json;

    /// Get-or-create by key. Keys are scoped to the binding, so two projects
    /// may use the same one. A key whose session was archived names a new
    /// session from then on.
    async fn open(&self, key: &str, title: Option<&str>) -> Result<SessionRef, SessionError>;

    /// Returns once the message is durably accepted: it starts a run when the
    /// session is idle, and waits for the session's next message moment when
    /// one is under way. A `dedupe` id already recorded against the session
    /// makes this a successful no-op. Only sessions this binding opened.
    async fn send(
        &self,
        session: &str,
        message: Message,
        dedupe: Option<&str>,
    ) -> Result<(), SendError>;

    /// Idempotent and soft. Only sessions this binding opened.
    async fn archive(&self, session: &str) -> Result<(), SessionError>;

    /// This binding's store: the same one its tool handlers get from
    /// [`DaemonHost::store_of`].
    fn storage(&self) -> SharedStore;
}

pub type SharedBinding = Arc<dyn BindingHandle>;

/// `variant event`.
#[derive(Clone)]
pub enum Event {
    /// A binding to serve: on every start, once per current binding, before
    /// anything else; later, when one is bound or enabled.
    Added(SharedBinding),
    /// Same id, new config. The previous handle now answers `gone`.
    Changed(SharedBinding),
    /// The binding's id. It was unbound, disabled, or moved to another
    /// version; its handle answers `gone`.
    Removed(String),
}

impl Event {
    /// The id of the binding the event is about.
    pub fn binding(&self) -> &str {
        match self {
            Event::Added(handle) | Event::Changed(handle) => handle.id(),
            Event::Removed(id) => id,
        }
    }
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Added(handle) => f.debug_tuple("Added").field(&handle.id()).finish(),
            Event::Changed(handle) => f.debug_tuple("Changed").field(&handle.id()).finish(),
            Event::Removed(id) => f.debug_tuple("Removed").field(id).finish(),
        }
    }
}

/// `stream<event>`: what `serve` waits on. It ends when the host asks for
/// shutdown.
#[derive(Debug)]
pub struct Events(mpsc::Receiver<Event>);

/// The host's end of an [`Events`] stream. Dropping every sender closes it,
/// which is how the host asks `serve` to return.
pub type EventSender = mpsc::Sender<Event>;

impl Events {
    /// A stream and its host end, holding at most `capacity` undelivered
    /// events.
    pub fn channel(capacity: usize) -> (EventSender, Events) {
        let (sender, receiver) = mpsc::channel(capacity.max(1));
        (sender, Events(receiver))
    }

    /// The next event, or `None` once the host has asked for shutdown.
    pub async fn next(&mut self) -> Option<Event> {
        self.0.recv().await
    }
}

/// The rest of `interface host`: what a tool handler of a daemon plugin calls
/// with the session it is serving. The binding is the one `ctx` belongs to,
/// never an argument.
#[async_trait]
pub trait DaemonHost: Send + Sync + 'static {
    /// The key this plugin's binding opened the calling session under; `None`
    /// when the session was not opened by it.
    async fn key_of(&self, ctx: &SessionCtx) -> Option<String>;

    /// The store the daemon sees for this binding.
    async fn store_of(&self, ctx: &SessionCtx) -> SharedStore;
}

// --- daemon ---

/// `interface daemon`.
///
/// One `serve` call is one instance's whole life. Memory is a cache: the host
/// may end the call at any moment and starts a fresh one, so anything that
/// must survive goes in a binding's store or in the host's session keys and
/// dedupe ids.
#[async_trait]
pub trait Daemon: Send + Sync + 'static {
    /// Runs until `events` ends, then should return within [`GRACE`]; the
    /// host drops it after that. Returning while bindings remain — ok or not —
    /// counts as a failure and is restarted with backoff. An error goes to the
    /// logs and to the UI of every bound project, never to a model.
    async fn serve(&self, events: Events) -> Result<(), String>;
}

pub type SharedDaemon = Arc<dyn Daemon>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Content;

    #[tokio::test]
    async fn compare_and_set_follows_if_version() {
        let store = MemoryStore::new(BINDING_CAP);
        let v1 = store.put("gateway", b"a".to_vec(), Some(0)).await.unwrap();
        assert_eq!(
            store.put("gateway", b"b".to_vec(), Some(0)).await,
            Err(StorageError::Conflict)
        );
        let v2 = store.put("gateway", b"b".to_vec(), Some(v1)).await.unwrap();
        assert_ne!(v1, v2);
        assert_eq!(
            store.put("gateway", b"c".to_vec(), Some(v1)).await,
            Err(StorageError::Conflict)
        );
        assert_eq!(
            store.delete("gateway", Some(v1)).await,
            Err(StorageError::Conflict)
        );
        store.delete("gateway", Some(v2)).await.unwrap();
        assert_eq!(store.get("gateway").await, Err(StorageError::NotFound));
        assert_eq!(
            store.delete("gateway", None).await,
            Err(StorageError::NotFound)
        );
    }

    #[tokio::test]
    async fn caps_refuse_without_evicting() {
        let store = MemoryStore::new(20);
        store.put("a", vec![0; 10], None).await.unwrap();
        assert_eq!(
            store.put("b", vec![0; 10], None).await,
            Err(StorageError::Quota)
        );
        // Replacing a record only counts the difference.
        store.put("a", vec![0; 19], None).await.unwrap();
        assert_eq!(store.usage().await.unwrap(), (20, 20));
        assert_eq!(store.get("a").await.unwrap().value.len(), 19);

        let big = MemoryStore::new(u64::MAX);
        assert_eq!(
            big.put("a", vec![0; RECORD_CAP + 1], None).await,
            Err(StorageError::TooLarge)
        );
        assert_eq!(
            big.put("", vec![], None).await,
            Err(StorageError::InvalidKey)
        );
        assert_eq!(
            big.put("tab\there", vec![], None).await,
            Err(StorageError::InvalidKey)
        );
    }

    #[tokio::test]
    async fn list_pages_in_key_order() {
        let store = MemoryStore::new(BINDING_CAP);
        for key in ["ch:3", "ch:1", "other", "ch:2"] {
            store.put(key, vec![], None).await.unwrap();
        }
        assert_eq!(store.list("ch:", None, 2).await.unwrap(), ["ch:1", "ch:2"]);
        assert_eq!(store.list("ch:", Some("ch:2"), 2).await.unwrap(), ["ch:3"]);
        store.close();
        assert_eq!(store.list("", None, 10).await, Err(StorageError::Gone));
    }

    #[test]
    fn send_refuses_what_a_notice_would_and_system() {
        assert_eq!(check_send(&Message::user("hi"), Some("1175")), Ok(()));
        assert_eq!(
            check_send(&Message::system("hi"), None),
            Err(SendError::RoleRefused)
        );
        assert_eq!(
            check_send(&Message::user("  "), None),
            Err(SendError::Empty)
        );
        let huge = Message {
            role: crate::types::Role::User,
            content: vec![Content::text("x".repeat(crate::notice::NOTICE_CAP + 1))],
        };
        assert_eq!(check_send(&huge, None), Err(SendError::TooLarge));
    }

    #[test]
    fn serialized_names_are_the_wit_ones() {
        assert_eq!(
            serde_json::to_value(SessionError::InvalidKey).unwrap(),
            serde_json::json!("invalid-key")
        );
        assert_eq!(
            serde_json::to_value(SendError::RoleRefused).unwrap(),
            serde_json::json!("role-refused")
        );
    }
}
