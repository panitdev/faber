//! Reading content-addressed bytes back out.
//!
//! `run` writes every blob inline in `blob.data`; `storage_path` is the
//! spill-to-disk column the schema reserves for payloads too large to keep in
//! the row, so a reader has to honour it even though nothing sets it yet.
//!
//! Nothing here is on the run path. This exists for the debug surfaces that
//! show the raw exchange a run recorded (`history-abstract.md` H2/H7).

use diesel::{OptionalExtension, QueryDsl, SelectableHelper};
use diesel_async::{AsyncPgConnection, RunQueryDsl};

use crate::{
    error::{ApiResult, AppError},
    models::blob::Blob,
    schema::blob,
};

/// The bytes behind a digest, or `None` when the row is gone.
///
/// A digest with neither inline data nor a path is treated as missing rather
/// than empty: returning `Some(vec![])` would render as a present-but-blank
/// exchange, which is the one answer that hides a real storage problem.
pub async fn read_blob(conn: &mut AsyncPgConnection, digest: &[u8]) -> ApiResult<Option<Vec<u8>>> {
    let record: Option<Blob> = blob::table
        .find(digest)
        .select(Blob::as_select())
        .first(conn)
        .await
        .optional()
        .map_err(|err| AppError::db(err, "blobs.read_blob"))?;

    let Some(record) = record else {
        return Ok(None);
    };

    if let Some(data) = record.data {
        return Ok(Some(data));
    }

    if let Some(path) = record.storage_path {
        match tokio::fs::read(&path).await {
            Ok(bytes) => return Ok(Some(bytes)),
            Err(error) => {
                tracing::error!(%error, path = %path, "blob spilled to disk could not be read");
                return Err(AppError::Internal);
            }
        }
    }

    Ok(None)
}
