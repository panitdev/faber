use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use crate::schema::exchange;

#[allow(dead_code)]
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = exchange)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Exchange {
    pub id: Uuid,
    pub run_id: Uuid,
    pub request_blob_digest: Vec<u8>,
    pub provider_events_digest: Option<Vec<u8>>,
    /// The canonical lineage this exchange committed, as a serialized
    /// `harness::Seed`. Set only on the exchange a `spine` row names — the
    /// rest are the garbage class (`history-abstract.md` H7).
    pub canonical_blob_digest: Option<Vec<u8>>,
    pub usage: Option<Value>,
    pub outcome: Option<Value>,
    pub expected_cache_tokens: i64,
    pub actual_cache_tokens: Option<i64>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    /// A serialized [`crate::timing::Timing`]. `None` on an exchange written
    /// before timing was measured, and on a call that was never sent.
    pub timing: Option<Value>,
}

/// An exchange is inserted once, complete.
///
/// The `exchange_append_only` trigger raises on every `UPDATE`, so there is no
/// insert-then-fill-in path: `usage`, `outcome`, and `completed_at` have to be
/// known before the row is written, which means an exchange is recorded after
/// its call has reached a terminal state, not when it opens.
#[derive(Insertable)]
#[diesel(table_name = exchange)]
pub struct NewExchange<'a> {
    pub id: Uuid,
    pub run_id: Uuid,
    pub request_blob_digest: &'a [u8],
    pub provider_events_digest: Option<&'a [u8]>,
    pub canonical_blob_digest: Option<&'a [u8]>,
    pub usage: Option<Value>,
    pub outcome: Option<Value>,
    pub expected_cache_tokens: i64,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub timing: Option<Value>,
}
