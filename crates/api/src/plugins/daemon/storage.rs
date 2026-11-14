//! Per-binding plugin storage, in Postgres (`binding_record`).
//!
//! One store per binding, and a binding is a plugin type in a project. The
//! host picks the store from the binding handle the daemon holds or from the
//! session a tool handler serves, so a plugin can never name another
//! binding's store. Every write is checked against three limits, and one past
//! any of them fails with `quota` or `too-large`; nothing is evicted:
//!
//! - one value: [`Limits::record_cap`];
//! - one binding's records: [`Limits::binding_cap`];
//! - every binding's records in the project: [`Limits::project_quota`].
//!
//! Writes to one project are serialized by an advisory lock, so two
//! concurrent writes cannot both pass the quota check on the same headroom.

use std::sync::Arc;

use async_trait::async_trait;
use diesel::sql_types::{BigInt, Bytea, Nullable, Text, Uuid as SqlUuid};
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use plugin::daemon::{
    self, Entry, LIST_CAP, RECORD_CAP, SharedStore, StorageError, Store, record_size, valid_key,
};
use uuid::Uuid;

use super::{BindingKey, Lease};
use crate::db::DbPool;

/// What one project's plugins may store, unless the operator says otherwise.
pub const DEFAULT_PROJECT_QUOTA: u64 = 256 * 1024 * 1024;

/// The operator's storage limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of one value.
    pub record_cap: usize,
    /// Bytes of one binding's records, keys included.
    pub binding_cap: u64,
    /// Bytes of every binding's records in one project.
    pub project_quota: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            record_cap: RECORD_CAP,
            binding_cap: daemon::BINDING_CAP,
            project_quota: DEFAULT_PROJECT_QUOTA,
        }
    }
}

impl Limits {
    /// `FABER_PLUGIN_STORAGE_QUOTA_BYTES` (per project) and
    /// `FABER_PLUGIN_STORAGE_BINDING_BYTES` (per binding), each a whole number
    /// of bytes; unset or blank is the default. A value that does not parse
    /// is refused, so a typo cannot quietly run on a limit nobody chose.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Limits, String> {
        let bytes = |name: &str, default: u64| -> Result<u64, String> {
            match var(name).as_deref().map(str::trim) {
                None | Some("") => Ok(default),
                Some(text) => text
                    .parse()
                    .map_err(|_| format!("{name} must be a whole number of bytes, not `{text}`")),
            }
        };
        let defaults = Limits::default();
        Ok(Limits {
            record_cap: defaults.record_cap,
            binding_cap: bytes("FABER_PLUGIN_STORAGE_BINDING_BYTES", defaults.binding_cap)?,
            project_quota: bytes("FABER_PLUGIN_STORAGE_QUOTA_BYTES", defaults.project_quota)?,
        })
    }

    pub fn from_env() -> Limits {
        Limits::from_vars(|name| std::env::var(name).ok())
            .unwrap_or_else(|message| panic!("{message}"))
    }
}

/// Hands out binding stores. Cheap to clone.
#[derive(Clone)]
pub struct Stores {
    db: DbPool,
    limits: Limits,
}

impl Stores {
    pub fn new(db: DbPool, limits: Limits) -> Self {
        Stores { db, limits }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The store of `binding`, answering `gone` once `lease` is retired. The
    /// daemon's handle and its store share one lease.
    pub fn for_handle(&self, binding: BindingKey, lease: Lease) -> SharedStore {
        Arc::new(PgStore {
            db: self.db.clone(),
            limits: self.limits,
            binding,
            lease: Some(lease),
        })
    }

    /// The store of `binding` for a tool handler: tied to no handle, so it
    /// is never `gone`.
    pub fn for_binding(&self, binding: BindingKey) -> SharedStore {
        Arc::new(PgStore {
            db: self.db.clone(),
            limits: self.limits,
            binding,
            lease: None,
        })
    }

    /// Bytes `binding`'s store holds.
    pub async fn used(&self, binding: &BindingKey) -> Result<u64, StorageError> {
        let mut conn = self.db.get().await.map_err(unavailable)?;
        binding_used(&mut conn, binding).await
    }

    /// Deletes every record of `binding`. What the owner's purge of an
    /// unbound binding's store does; returns how many records went.
    pub async fn purge(&self, binding: &BindingKey) -> Result<usize, StorageError> {
        let mut conn = self.db.get().await.map_err(unavailable)?;
        diesel::sql_query("DELETE FROM binding_record WHERE project_id = $1 AND plugin_type = $2")
            .bind::<SqlUuid, _>(binding.project)
            .bind::<Text, _>(&binding.plugin)
            .execute(&mut conn)
            .await
            .map_err(unavailable)
    }
}

struct PgStore {
    db: DbPool,
    limits: Limits,
    binding: BindingKey,
    lease: Option<Lease>,
}

impl PgStore {
    fn live(&self) -> Result<(), StorageError> {
        match &self.lease {
            Some(lease) if lease.is_retired() => Err(StorageError::Gone),
            _ => Ok(()),
        }
    }

    async fn conn(
        &self,
    ) -> Result<
        diesel_async::pooled_connection::bb8::PooledConnection<'_, AsyncPgConnection>,
        StorageError,
    > {
        self.live()?;
        self.db.get().await.map_err(unavailable)
    }
}

fn unavailable(error: impl std::fmt::Display) -> StorageError {
    tracing::warn!(%error, "plugin storage call failed");
    StorageError::Unavailable
}

/// Rows of the raw queries below.
// The derive expands each field as `field: field`, which clippy flags.
#[allow(clippy::redundant_field_names)]
mod rows {
    use diesel::QueryableByName;
    use diesel::sql_types::{BigInt, Bytea, Nullable, Text};

    #[derive(QueryableByName)]
    pub struct Record {
        #[diesel(sql_type = Bytea)]
        pub value: Vec<u8>,
        #[diesel(sql_type = BigInt)]
        pub version: i64,
    }

    #[derive(QueryableByName)]
    pub struct Current {
        #[diesel(sql_type = BigInt)]
        pub version: i64,
        #[diesel(sql_type = BigInt)]
        pub size: i64,
    }

    #[derive(QueryableByName)]
    pub struct Total {
        #[diesel(sql_type = Nullable<BigInt>)]
        pub total: Option<i64>,
    }

    #[derive(QueryableByName)]
    pub struct Key {
        #[diesel(sql_type = Text)]
        pub key: String,
    }

    #[derive(QueryableByName)]
    pub struct Version {
        #[diesel(sql_type = BigInt)]
        pub version: i64,
    }
}

use rows::{Current, Key, Record, Total, Version};

async fn binding_used(
    conn: &mut AsyncPgConnection,
    binding: &BindingKey,
) -> Result<u64, StorageError> {
    let total: Total = diesel::sql_query(
        "SELECT sum(size)::bigint AS total FROM binding_record \
         WHERE project_id = $1 AND plugin_type = $2",
    )
    .bind::<SqlUuid, _>(binding.project)
    .bind::<Text, _>(&binding.plugin)
    .get_result(conn)
    .await
    .map_err(unavailable)?;
    Ok(total.total.unwrap_or(0).max(0) as u64)
}

async fn project_used(conn: &mut AsyncPgConnection, project: Uuid) -> Result<u64, StorageError> {
    let total: Total = diesel::sql_query(
        "SELECT sum(size)::bigint AS total FROM binding_record WHERE project_id = $1",
    )
    .bind::<SqlUuid, _>(project)
    .get_result(conn)
    .await
    .map_err(unavailable)?;
    Ok(total.total.unwrap_or(0).max(0) as u64)
}

/// Serializes writes to one project's stores until the transaction ends.
async fn lock_project(conn: &mut AsyncPgConnection, project: Uuid) -> Result<(), StorageError> {
    diesel::sql_query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind::<Text, _>(format!("binding_record:{project}"))
        .execute(conn)
        .await
        .map(|_| ())
        .map_err(unavailable)
}

async fn current(
    conn: &mut AsyncPgConnection,
    binding: &BindingKey,
    key: &str,
) -> Result<Option<Current>, StorageError> {
    use diesel::OptionalExtension;
    diesel::sql_query(
        "SELECT version, size FROM binding_record \
         WHERE project_id = $1 AND plugin_type = $2 AND key = $3 FOR UPDATE",
    )
    .bind::<SqlUuid, _>(binding.project)
    .bind::<Text, _>(&binding.plugin)
    .bind::<Text, _>(key)
    .get_result(conn)
    .await
    .optional()
    .map_err(unavailable)
}

/// Whether a record at `current` may be written under `wanted`.
fn matches(current: Option<u64>, wanted: Option<u64>) -> bool {
    match wanted {
        None => true,
        Some(0) => current.is_none(),
        Some(version) => current == Some(version),
    }
}

/// What a transaction body fails with: the store's answer, or the database's.
enum Failed {
    Store(StorageError),
    Db(diesel::result::Error),
}

impl From<diesel::result::Error> for Failed {
    fn from(error: diesel::result::Error) -> Self {
        Failed::Db(error)
    }
}

impl From<StorageError> for Failed {
    fn from(error: StorageError) -> Self {
        Failed::Store(error)
    }
}

impl From<Failed> for StorageError {
    fn from(failed: Failed) -> Self {
        match failed {
            Failed::Store(error) => error,
            Failed::Db(error) => unavailable(error),
        }
    }
}

#[async_trait]
impl Store for PgStore {
    async fn get(&self, key: &str) -> Result<Entry, StorageError> {
        if !valid_key(key) {
            self.live()?;
            return Err(StorageError::InvalidKey);
        }
        let mut conn = self.conn().await?;
        use diesel::OptionalExtension;
        let record: Option<Record> = diesel::sql_query(
            "SELECT value, version FROM binding_record \
             WHERE project_id = $1 AND plugin_type = $2 AND key = $3",
        )
        .bind::<SqlUuid, _>(self.binding.project)
        .bind::<Text, _>(&self.binding.plugin)
        .bind::<Text, _>(key)
        .get_result(&mut conn)
        .await
        .optional()
        .map_err(unavailable)?;
        let record = record.ok_or(StorageError::NotFound)?;
        Ok(Entry {
            key: key.to_owned(),
            value: record.value,
            version: record.version as u64,
        })
    }

    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
        if_version: Option<u64>,
    ) -> Result<u64, StorageError> {
        self.live()?;
        if !valid_key(key) {
            return Err(StorageError::InvalidKey);
        }
        if value.len() > self.limits.record_cap {
            return Err(StorageError::TooLarge);
        }
        let mut conn = self.conn().await?;
        let binding = self.binding.clone();
        let limits = self.limits;
        let key = key.to_owned();
        let written = conn
            .transaction::<u64, Failed, _>(move |conn| {
                async move {
                    lock_project(conn, binding.project).await?;
                    let found = current(conn, &binding, &key).await?;
                    if !matches(found.as_ref().map(|c| c.version as u64), if_version) {
                        return Err(StorageError::Conflict.into());
                    }
                    let freed = found.as_ref().map_or(0, |c| c.size.max(0) as u64);
                    let size = record_size(&key, &value);
                    let in_binding = binding_used(conn, &binding).await? - freed + size;
                    let in_project = project_used(conn, binding.project).await? - freed + size;
                    if in_binding > limits.binding_cap || in_project > limits.project_quota {
                        return Err(StorageError::Quota.into());
                    }
                    let version: Version = diesel::sql_query(
                        "INSERT INTO binding_record (project_id, plugin_type, key, value) \
                         VALUES ($1, $2, $3, $4) \
                         ON CONFLICT (project_id, plugin_type, key) DO UPDATE \
                         SET value = EXCLUDED.value, \
                             version = nextval('binding_record_version_seq'), \
                             updated_at = now() \
                         RETURNING version",
                    )
                    .bind::<SqlUuid, _>(binding.project)
                    .bind::<Text, _>(&binding.plugin)
                    .bind::<Text, _>(&key)
                    .bind::<Bytea, _>(&value)
                    .get_result(conn)
                    .await?;
                    Ok(version.version as u64)
                }
                .scope_boxed()
            })
            .await?;
        Ok(written)
    }

    async fn delete(&self, key: &str, if_version: Option<u64>) -> Result<(), StorageError> {
        self.live()?;
        if !valid_key(key) {
            return Err(StorageError::InvalidKey);
        }
        let mut conn = self.conn().await?;
        let binding = self.binding.clone();
        let key = key.to_owned();
        conn.transaction::<(), Failed, _>(move |conn| {
            async move {
                let found = current(conn, &binding, &key)
                    .await?
                    .ok_or(StorageError::NotFound)?;
                if if_version.is_some_and(|wanted| wanted != found.version as u64) {
                    return Err(StorageError::Conflict.into());
                }
                diesel::sql_query(
                    "DELETE FROM binding_record \
                     WHERE project_id = $1 AND plugin_type = $2 AND key = $3",
                )
                .bind::<SqlUuid, _>(binding.project)
                .bind::<Text, _>(&binding.plugin)
                .bind::<Text, _>(&key)
                .execute(conn)
                .await?;
                Ok(())
            }
            .scope_boxed()
        })
        .await?;
        Ok(())
    }

    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<String>, StorageError> {
        let mut conn = self.conn().await?;
        let limit = i64::from(limit.min(LIST_CAP));
        // Byte order, whatever the database's collation: keys are ASCII, and
        // `after` paging must agree with the order rows come back in.
        let keys: Vec<Key> = diesel::sql_query(
            "SELECT key FROM binding_record \
             WHERE project_id = $1 AND plugin_type = $2 \
               AND starts_with(key, $3) \
               AND ($4::text IS NULL OR key COLLATE \"C\" > $4::text COLLATE \"C\") \
             ORDER BY key COLLATE \"C\" LIMIT $5",
        )
        .bind::<SqlUuid, _>(self.binding.project)
        .bind::<Text, _>(&self.binding.plugin)
        .bind::<Text, _>(prefix)
        .bind::<Nullable<Text>, _>(after)
        .bind::<BigInt, _>(limit)
        .load(&mut conn)
        .await
        .map_err(unavailable)?;
        Ok(keys.into_iter().map(|row| row.key).collect())
    }

    async fn usage(&self) -> Result<(u64, u64), StorageError> {
        let mut conn = self.conn().await?;
        let in_binding = binding_used(&mut conn, &self.binding).await?;
        let in_project = project_used(&mut conn, self.binding.project).await?;
        let project_left = self.limits.project_quota.saturating_sub(in_project);
        let allowed = self.limits.binding_cap.min(in_binding + project_left);
        Ok((in_binding, allowed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::daemon::fixture::Fixture;

    #[test]
    fn limits_come_from_their_variables() {
        let limits = Limits::from_vars(|name| match name {
            "FABER_PLUGIN_STORAGE_QUOTA_BYTES" => Some(" 1000 ".into()),
            "FABER_PLUGIN_STORAGE_BINDING_BYTES" => Some("".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(limits.project_quota, 1000);
        assert_eq!(limits.binding_cap, daemon::BINDING_CAP);
        let refused = Limits::from_vars(|_| Some("16M".into())).unwrap_err();
        assert!(refused.contains("16M"), "{refused}");
    }

    fn stores(fixture: &Fixture, limits: Limits) -> Stores {
        Stores::new(fixture.pool.clone(), limits)
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn compare_and_set_and_create_only() {
        let fixture = Fixture::new(0).await;
        let store = stores(&fixture, Limits::default()).for_binding(fixture.binding("discord"));

        let v1 = store.put("gateway", b"a".to_vec(), Some(0)).await.unwrap();
        assert!(v1 > 0);
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
        let v3 = store.put("gateway", b"c".to_vec(), None).await.unwrap();
        let entry = store.get("gateway").await.unwrap();
        assert_eq!((entry.value.as_slice(), entry.version), (&b"c"[..], v3));

        assert_eq!(
            store.delete("gateway", Some(v2)).await,
            Err(StorageError::Conflict)
        );
        store.delete("gateway", Some(v3)).await.unwrap();
        assert_eq!(store.get("gateway").await, Err(StorageError::NotFound));
        assert_eq!(
            store.delete("gateway", None).await,
            Err(StorageError::NotFound)
        );
        assert_eq!(store.get("bad\nkey").await, Err(StorageError::InvalidKey));
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn list_pages_in_key_order_within_one_binding() {
        let fixture = Fixture::new(0).await;
        let stores = stores(&fixture, Limits::default());
        let store = stores.for_binding(fixture.binding("discord"));
        let other = stores.for_binding(fixture.binding("slack"));
        for key in ["ch:B", "ch:a", "ch:1", "other"] {
            store.put(key, vec![], None).await.unwrap();
        }
        other.put("ch:zz", vec![], None).await.unwrap();

        // Byte order: digits, then upper case, then lower case.
        assert_eq!(store.list("ch:", None, 2).await.unwrap(), ["ch:1", "ch:B"]);
        assert_eq!(store.list("ch:", Some("ch:B"), 2).await.unwrap(), ["ch:a"]);
        assert_eq!(store.list("", None, 10).await.unwrap().len(), 4);
        assert_eq!(other.list("", None, 10).await.unwrap(), ["ch:zz"]);
        // `%` and `_` are not patterns.
        assert!(store.list("ch%", None, 10).await.unwrap().is_empty());
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn caps_and_the_project_quota_refuse_without_evicting() {
        let fixture = Fixture::new(0).await;
        let stores = stores(
            &fixture,
            Limits {
                record_cap: 100,
                binding_cap: 150,
                project_quota: 200,
            },
        );
        let discord = stores.for_binding(fixture.binding("discord"));
        let slack = stores.for_binding(fixture.binding("slack"));

        assert_eq!(
            discord.put("a", vec![0; 101], None).await,
            Err(StorageError::TooLarge)
        );
        // 1 + 99 bytes, then 1 + 60 would pass the binding's 150.
        discord.put("a", vec![0; 99], None).await.unwrap();
        assert_eq!(
            discord.put("b", vec![0; 60], None).await,
            Err(StorageError::Quota)
        );
        // Replacing a record counts only the difference.
        discord.put("a", vec![0; 100], None).await.unwrap();
        assert_eq!(discord.usage().await.unwrap(), (101, 150));

        // The project's 200 is shared: slack has 99 left, not its own 150.
        assert_eq!(slack.usage().await.unwrap(), (0, 99));
        slack.put("x", vec![0; 98], None).await.unwrap();
        assert_eq!(slack.put("y", vec![], None).await, Err(StorageError::Quota));
        // Nothing was evicted to make room.
        assert_eq!(discord.get("a").await.unwrap().value.len(), 100);
        assert_eq!(stores.used(&fixture.binding("discord")).await.unwrap(), 101);

        assert_eq!(stores.purge(&fixture.binding("discord")).await.unwrap(), 1);
        slack.put("y", vec![], None).await.unwrap();
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn the_daemon_and_tool_handlers_share_a_store_and_a_retired_handle_is_gone() {
        let fixture = Fixture::new(0).await;
        let stores = stores(&fixture, Limits::default());
        let lease = Lease::new();
        let daemon = stores.for_handle(fixture.binding("discord"), lease.clone());
        let tools = stores.for_binding(fixture.binding("discord"));

        let version = daemon.put("seq", b"42".to_vec(), None).await.unwrap();
        assert_eq!(tools.get("seq").await.unwrap().version, version);
        tools
            .put("seq", b"43".to_vec(), Some(version))
            .await
            .unwrap();
        assert_eq!(daemon.get("seq").await.unwrap().value, b"43");

        lease.retire();
        assert_eq!(daemon.get("seq").await, Err(StorageError::Gone));
        assert_eq!(
            daemon.put("seq", vec![], None).await,
            Err(StorageError::Gone)
        );
        assert_eq!(daemon.list("", None, 1).await, Err(StorageError::Gone));
        // The record outlives the handle; a fresh one reads it.
        assert_eq!(tools.get("seq").await.unwrap().value, b"43");
        fixture.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "needs the database: set DATABASE_URL"]
    async fn concurrent_writes_cannot_share_the_same_headroom() {
        let fixture = Fixture::new(0).await;
        let stores = stores(
            &fixture,
            Limits {
                record_cap: 100,
                binding_cap: 1000,
                project_quota: 100,
            },
        );
        let store = stores.for_binding(fixture.binding("discord"));
        let writes = (0..8).map(|i| {
            let store = Arc::clone(&store);
            async move { store.put(&format!("k{i}"), vec![0; 48], None).await }
        });
        let results = futures::future::join_all(writes).await;
        let ok = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(ok, 2, "{results:?}");
        assert!(stores.used(&fixture.binding("discord")).await.unwrap() <= 100);
        fixture.cleanup().await;
    }
}
