//! Bounded reports of byte-identical files from successful scan observations.

use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;

/// One observed file sharing its digest with another catalog file.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct DuplicateFile {
    /// Lowercase SHA-256 of complete file bytes, including embedded metadata.
    pub(crate) sha256: String,
    /// Stable file identifier; identities are never automatically merged.
    pub(crate) file_id: i64,
    /// Recording currently associated with this path.
    pub(crate) recording_id: i64,
    /// Source path as observed by the scanner.
    pub(crate) path: String,
}

/// List duplicate observations by digest and ID, with file-bounded pagination.
///
/// Results describe the last successful probes, not a live filesystem check.
/// Removed paths remain until catalog cleanup is explicitly implemented.
pub(crate) async fn list(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<DuplicateFile>, sqlx::Error> {
    sqlx::query_as(
        "SELECT h.sha256, f.id AS file_id, f.recording_id, f.path \
         FROM audio_file_hashes h JOIN audio_files f ON f.id = h.file_id \
         WHERE h.sha256 IN (SELECT sha256 FROM audio_file_hashes \
             GROUP BY sha256 HAVING COUNT(*) > 1) \
         ORDER BY h.sha256, f.id LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}
