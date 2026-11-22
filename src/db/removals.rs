//! Durable file-removal intent and atomic catalog completion.

use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;

/// Administrator-visible removal history; paths and hashes stay internal.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct Removal {
    /// Stable operation identifier.
    pub(crate) id: i64,
    /// Preserved recording; personal references are never deleted.
    pub(crate) recording_id: i64,
    /// Original file identifier, retained after unregistering it.
    pub(crate) file_id: i64,
    /// Whether server-owned bytes should also be removed.
    pub(crate) remove_file: bool,
    /// Account that requested this shared mutation, if still present.
    pub(crate) requested_by: Option<i64>,
    /// pending, completed or failed.
    pub(crate) status: String,
    /// Unix admission time.
    pub(crate) created_at: i64,
    /// Unix completion time, if finished.
    pub(crate) finished_at: Option<i64>,
    /// Safe outcome description; never a raw filesystem error.
    pub(crate) message: Option<String>,
}

/// Private immutable identity captured before any filesystem operation.
#[derive(sqlx::FromRow)]
pub(crate) struct Intent {
    /// Durable operation identity.
    pub(crate) id: i64,
    /// Server-registered absolute object path.
    pub(crate) path: String,
    /// Previously measured content identity, required for physical removal.
    pub(crate) sha256: Option<String>,
    /// Unregister-only operations never access the filesystem.
    pub(crate) remove_file: bool,
}

/// Persist one intent; the caller holds the process-wide admission mutex.
/// Only confirmed acquired objects can request physical removal. A global
/// partial index also prevents concurrent admission across server processes.
pub(crate) async fn start(
    pool: &SqlitePool,
    recording: i64,
    file: i64,
    user: i64,
    remove_file: bool,
) -> Result<Option<Intent>, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO file_removals \
        (recording_id,file_id,path,sha256,remove_file,requested_by) \
        SELECT f.recording_id,f.id,f.path,h.sha256,?3,?4 \
        FROM audio_files f LEFT JOIN audio_file_hashes h ON h.file_id=f.id \
        WHERE f.recording_id=?1 AND f.id=?2 \
        AND NOT EXISTS(SELECT 1 FROM file_removals WHERE status='pending') \
        AND NOT EXISTS(SELECT 1 FROM download_imports i \
            JOIN download_jobs j ON j.id=i.job_id \
            WHERE i.path=f.path AND j.status='importing') \
        AND (?3=0 OR (h.sha256 IS NOT NULL AND EXISTS( \
            SELECT 1 FROM downloaded_sources s WHERE s.file_id=f.id))) \
        RETURNING id,path,sha256,remove_file",
    )
    .bind(recording)
    .bind(file)
    .bind(remove_file)
    .bind(user)
    .fetch_optional(pool)
    .await
}

/// Read the interrupted intent before serving downloads or streaming.
pub(crate) async fn pending(
    pool: &SqlitePool,
) -> Result<Option<Intent>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,path,sha256,remove_file FROM file_removals \
        WHERE status='pending'",
    )
    .fetch_optional(pool)
    .await
}

/// Atomically unregister a file and complete its audit event.
pub(crate) async fn complete(
    pool: &SqlitePool,
    id: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(
        "DELETE FROM audio_files WHERE id=(SELECT file_id \
        FROM file_removals WHERE id=? AND status='pending')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE file_removals SET status='completed', \
        finished_at=unixepoch() WHERE id=? AND status='pending'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Keep the file registered when validation or physical removal fails.
pub(crate) async fn fail(
    pool: &SqlitePool,
    id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE file_removals SET status='failed', \
        finished_at=unixepoch(),message='File removal could not be completed' \
        WHERE id=? AND status='pending'",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Read one administrator-visible operation.
pub(crate) async fn find(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<Removal>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,recording_id,file_id,remove_file,requested_by, \
        status,created_at,finished_at,message FROM file_removals WHERE id=?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Read newest-first administrative history in a bounded page.
pub(crate) async fn list(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Removal>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id,recording_id,file_id,remove_file,requested_by, \
        status,created_at,finished_at,message FROM file_removals \
        ORDER BY id DESC LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}
