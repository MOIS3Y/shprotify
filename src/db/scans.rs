//! Durable scan admission, progress, restart recovery, and report pagination.

use std::path::PathBuf;

use sqlx::SqlitePool;

use crate::jobs::{ScanIssue, ScanJob, StartError};

/// Atomically admit one scan and capture the configured canonical roots.
pub(crate) async fn create(
    pool: &SqlitePool,
    roots: &[PathBuf],
    force: bool,
) -> Result<ScanJob, StartError> {
    let roots: Vec<_> =
        roots.iter().map(|path| path.to_string_lossy()).collect();
    let result = sqlx::query_as(
        "INSERT INTO library_scans (status, roots, force) \
         VALUES ('running', ?, ?) \
         RETURNING *",
    )
    .bind(sqlx::types::Json(roots))
    .bind(force)
    .fetch_one(pool)
    .await;
    match result {
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|cause| cause.is_unique_violation()) =>
        {
            Err(StartError::Busy)
        }
        other => other.map_err(StartError::from),
    }
}

/// Mark unfinished jobs as interrupted once, before serving new API requests.
///
/// Committed files remain indexed. Jobs are not silently resumed with a
/// potentially different configuration; the administrator can start a new one.
pub(crate) async fn recover(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE library_scans SET status = 'interrupted', \
         finished_at = CURRENT_TIMESTAMP, \
         message = 'Server restarted before indexing finished' \
         WHERE status = 'running'",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Read a persisted snapshot, including jobs from previous server processes.
pub(crate) async fn find(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<ScanJob>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM library_scans WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Read history in stable ascending ID order using validated bounds.
pub(crate) async fn list(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<ScanJob>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM library_scans ORDER BY id LIMIT ? OFFSET ?")
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
}

/// Record a failed probe, invalidate its old observation, and count the error.
pub(crate) async fn record_error(
    pool: &SqlitePool,
    id: i64,
    path: &str,
    message: &str,
) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "DELETE FROM audio_file_hashes WHERE file_id IN \
        (SELECT id FROM audio_files WHERE path = ?)",
    )
    .bind(path)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "DELETE FROM audio_file_states WHERE file_id IN \
        (SELECT id FROM audio_files WHERE path = ?)",
    )
    .bind(path)
    .execute(&mut *transaction)
    .await?;
    record_error_in(&mut transaction, id, path, message).await?;
    transaction.commit().await
}

/// Persist an issue and its counter inside an existing indexing transaction.
pub(super) async fn record_error_in(
    connection: &mut sqlx::SqliteConnection,
    id: i64,
    path: &str,
    message: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO library_scan_errors (scan_id, path, message) \
         VALUES (?, ?, ?)",
    )
    .bind(id)
    .bind(path)
    .bind(message)
    .execute(&mut *connection)
    .await?;
    sqlx::query("UPDATE library_scans SET errors = errors + 1 WHERE id = ?")
        .bind(id)
        .execute(connection)
        .await?;
    Ok(())
}

/// Read one job's error page; the HTTP layer separately checks job existence.
pub(crate) async fn errors(
    pool: &SqlitePool,
    id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<ScanIssue>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, path, message FROM library_scan_errors \
         WHERE scan_id = ? ORDER BY id LIMIT ? OFFSET ?",
    )
    .bind(id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Persist terminal status and final counters after the worker joins.
pub(crate) async fn finish(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    skipped: i64,
    unchanged: i64,
    message: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE library_scans SET status = ?, skipped = ?, unchanged = ?, \
         message = ?, \
         finished_at = CURRENT_TIMESTAMP WHERE id = ? AND status = 'running'",
    )
    .bind(status)
    .bind(skipped)
    .bind(unchanged)
    .bind(message)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Concurrent admission and durable restart recovery.

    use super::*;
    use crate::jobs::ScanStatus;

    /// Racing admissions have one winner; recovery survives reopening SQLite.
    #[tokio::test]
    async fn admits_one_scan_and_preserves_interrupted_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.db");
        let pool = crate::db::connect(&path).await.unwrap();
        let roots = vec![directory.path().to_owned()];
        let (first, second) = tokio::join!(
            create(&pool, &roots, false),
            create(&pool, &roots, false),
        );
        let job = match (first, second) {
            (Ok(job), Err(StartError::Busy))
            | (Err(StartError::Busy), Ok(job)) => job,
            other => panic!("Unexpected admission results: {other:?}"),
        };
        record_error(&pool, job.id, "/music/broken", "bad header")
            .await
            .unwrap();
        pool.close().await;
        let pool = crate::db::connect(&path).await.unwrap();
        recover(&pool).await.unwrap();
        let recovered = find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(recovered.status, ScanStatus::Interrupted);
        assert_eq!(recovered.errors, 1);
        assert_eq!(recovered.roots, [directory.path().to_str().unwrap()]);
        assert!(recovered.finished_at.is_some());
        let report = errors(&pool, job.id, 1, 0).await.unwrap();
        assert_eq!(report[0].path, "/music/broken");
        assert!(errors(&pool, job.id, 1, 1).await.unwrap().is_empty());
        let new = create(&pool, &[], false).await.unwrap();
        finish(&pool, new.id, "completed", 5, 0, None)
            .await
            .unwrap();
        recover(&pool).await.unwrap();
        let new = find(&pool, new.id).await.unwrap().unwrap();
        assert_eq!(new.status, ScanStatus::Completed);
        assert_eq!(new.skipped, 5);
        assert_eq!(list(&pool, 1, 1).await.unwrap()[0].id, new.id);
        pool.close().await;
    }
}
