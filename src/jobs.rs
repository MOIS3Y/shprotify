//! Bounded background library indexing with persisted job history.
//!
//! One scan runs at a time. Filesystem/probe work stays on a blocking worker;
//! each observation is committed separately, so readers remain responsive
//! and completed imports survive a later failure. Graceful shutdown waits for
//! accepted work before closing SQLite. Startup explicitly recovers old jobs.

use std::{path::PathBuf, sync::Arc};

use serde::Serialize;
use sqlx::SqlitePool;
use tokio::{sync::Mutex, task::JoinSet};
use utoipa::ToSchema;

use crate::{db, library::scan};

/// Durable scan lifecycle; completed jobs may have isolated file errors.
#[derive(Debug, Serialize, sqlx::Type, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub(crate) enum ScanStatus {
    /// Accepted and still being processed.
    Running,
    /// Traversal finished; inspect `errors` for rejected files.
    Completed,
    /// Fatal worker or storage failure stopped traversal.
    Failed,
    /// The server restarted before the job reached a terminal state.
    Interrupted,
}

/// Persisted job snapshot returned by the HTTP API.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct ScanJob {
    /// Stable positive job identifier.
    pub(crate) id: i64,
    /// Current lifecycle state.
    pub(crate) status: ScanStatus,
    /// Canonical collection roots captured when the job was accepted.
    #[sqlx(json)]
    pub(crate) roots: Vec<String>,
    /// New files and independent recordings committed so far.
    pub(crate) imported: i64,
    /// Existing paths whose measured properties and tags were refreshed.
    pub(crate) refreshed: i64,
    /// Unsupported entries; final count becomes available on completion.
    pub(crate) skipped: i64,
    /// Entry errors and matching issues; an imported file can have an issue.
    pub(crate) errors: i64,
    /// UTC creation time in SQLite's `YYYY-MM-DD HH:MM:SS` format.
    pub(crate) created_at: String,
    /// UTC terminal time in the same format, or null while running.
    pub(crate) finished_at: Option<String>,
    /// Safe explanation of fatal failure or restart interruption.
    pub(crate) message: Option<String>,
}

/// One recoverable scan failure, available in a bounded error page.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct ScanIssue {
    /// Stable issue identifier, ordered within the report.
    pub(crate) id: i64,
    /// Source path, displayed lossily if its bytes are not UTF-8.
    pub(crate) path: String,
    /// Filesystem/probe diagnostic, validation, or association issue.
    pub(crate) message: String,
}

/// Expected admission failures are distinct from unexpected storage failures.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StartError {
    /// The instance already has an active scan.
    #[error("A library scan is already running")]
    Busy,
    /// Shutdown no longer accepts new jobs.
    #[error("The library scanner is shutting down")]
    Stopping,
    /// Unexpected database diagnostics, reserved for server logs.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Shared scan executor; cloning it preserves admission and shutdown state.
#[derive(Clone)]
pub(crate) struct ScanService {
    /// Pool kept open until all accepted jobs finish.
    pool: SqlitePool,
    /// Validated roots shared by the HTTP router and worker tasks.
    roots: Arc<Vec<PathBuf>>,
    /// Serializes admission with shutdown and owns every worker supervisor.
    state: Arc<Mutex<Workers>>,
}

/// Mutable task lifecycle protected by the service admission mutex.
#[derive(Default)]
struct Workers {
    /// Once set, new jobs cannot race with pool shutdown.
    stopping: bool,
    /// Supervisors await blocking workers and persist final status.
    tasks: JoinSet<()>,
}

impl ScanService {
    /// Build an idle service from roots already checked during startup.
    pub(crate) fn new(pool: SqlitePool, roots: Vec<PathBuf>) -> Self {
        Self {
            pool,
            roots: Arc::new(roots),
            state: Arc::new(Mutex::new(Workers::default())),
        }
    }

    /// Persist admission before spawning; a unique index rejects racing scans.
    pub(crate) async fn start(&self) -> Result<ScanJob, StartError> {
        let mut state = self.state.lock().await;
        if state.stopping {
            return Err(StartError::Stopping);
        }
        while let Some(result) = state.tasks.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error, "Scan supervisor failed");
            }
        }
        let job = db::scans::create(&self.pool, &self.roots).await?;
        let pool = self.pool.clone();
        let roots = self.roots.clone();
        let id = job.id;
        state
            .tasks
            .spawn(async move { execute(pool, roots, id).await });
        Ok(job)
    }

    /// Stop accepting work and wait for all imports before pool closure.
    pub(crate) async fn close(&self) {
        let mut state = self.state.lock().await;
        state.stopping = true;
        while let Some(result) = state.tasks.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error, "Scan supervisor failed at shutdown");
            }
        }
    }
}

/// Supervise one blocking traversal and always attempt terminal persistence.
async fn execute(pool: SqlitePool, roots: Arc<Vec<PathBuf>>, id: i64) {
    tracing::info!(scan_id = id, "Library indexing started");
    let worker_pool = pool.clone();
    let runtime = tokio::runtime::Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        scan::run(&roots, |event| {
            runtime.block_on(process(&worker_pool, id, event))
        })
    })
    .await;
    let (status, skipped, message) = match result {
        Ok(Ok(summary)) => ("completed", summary.skipped as i64, None),
        failure => {
            tracing::error!(scan_id = id, error = ?failure,
                "Library indexing failed");
            (
                "failed",
                0,
                Some("Library indexing failed; see server logs"),
            )
        }
    };
    if let Err(error) =
        db::scans::finish(&pool, id, status, skipped, message).await
    {
        tracing::error!(scan_id = id, %error, "Failed to finish scan job");
    } else {
        tracing::info!(scan_id = id, status, "Library indexing finished");
    }
}

/// Index audio or persist an isolated entry error; unexpected SQL errors stop.
async fn process(
    pool: &SqlitePool,
    id: i64,
    event: scan::Event,
) -> anyhow::Result<()> {
    match event {
        scan::Event::Audio { file } => {
            let path = file.path.clone();
            match db::indexing::store(pool, id, *file).await {
                Ok(()) => Ok(()),
                Err(db::indexing::IndexError::Invalid(message)) => {
                    db::scans::record_error(pool, id, &path, message).await?;
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        }
        scan::Event::Error { path, message } => {
            db::scans::record_error(pool, id, &path, &message).await?;
            Ok(())
        }
        scan::Event::Summary { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    //! Worker supervision, repeat imports, shutdown, and safe fatal failures.

    use std::fs;

    use super::*;

    /// Run real probes on the worker and commit isolated successes and errors.
    #[tokio::test]
    async fn indexes_in_background_and_waits_for_work_at_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let music = directory.path().join("music");
        fs::create_dir(&music).unwrap();
        let audio = music.join("song.wav");
        let bytes = crate::library::metadata::tests::wave();
        fs::write(&audio, &bytes).unwrap();
        fs::write(music.join("bad.flac"), b"damaged").unwrap();
        fs::write(music.join("cover.txt"), b"keep").unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let service = ScanService::new(pool.clone(), vec![music.clone()]);
        let job = service.start().await.unwrap();
        service.close().await;
        let job = db::scans::find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(job.status, ScanStatus::Completed);
        assert_eq!(job.imported, 1);
        assert_eq!(job.refreshed, 0);
        assert_eq!(job.skipped, 1);
        assert_eq!(job.errors, 1);
        assert!(job.finished_at.is_some());
        assert!(matches!(service.start().await, Err(StartError::Stopping)));
        assert_eq!(fs::read(audio).unwrap(), bytes);
        assert_eq!(fs::read_dir(&music).unwrap().count(), 3);
        let service = ScanService::new(pool.clone(), vec![music]);
        let repeat = service.start().await.unwrap();
        service.close().await;
        let repeat = db::scans::find(&pool, repeat.id).await.unwrap().unwrap();
        assert_eq!(repeat.imported, 0);
        assert_eq!(repeat.refreshed, 1);
        pool.close().await;
    }

    /// A storage failure terminates the job without leaking SQL diagnostics.
    #[tokio::test]
    async fn persists_safe_fatal_status_and_rolls_back_failed_file() {
        let directory = tempfile::tempdir().unwrap();
        let music = directory.path().join("music");
        fs::create_dir(&music).unwrap();
        fs::write(
            music.join("song.wav"),
            crate::library::metadata::tests::wave(),
        )
        .unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_tags BEFORE INSERT \
            ON audio_file_tags BEGIN SELECT RAISE(ABORT, 'password'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let service = ScanService::new(pool.clone(), vec![music]);
        let job = service.start().await.unwrap();
        service.close().await;
        let job = db::scans::find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(job.status, ScanStatus::Failed);
        assert_eq!(job.imported, 0);
        assert_eq!(
            job.message.as_deref(),
            Some("Library indexing failed; see server logs")
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        pool.close().await;
    }

    /// Invalid source metadata is an isolated error rather than a fatal job.
    #[tokio::test]
    async fn continues_after_invalid_catalog_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let job = db::scans::create(&pool, &[]).await.unwrap();
        let path = directory.path().join("song.wav");
        fs::write(&path, crate::library::metadata::tests::wave()).unwrap();
        let mut source =
            crate::library::metadata::read(&path).unwrap().unwrap();
        source.tags.title = Some("a".repeat(256));
        process(
            &pool,
            job.id,
            scan::Event::Audio {
                file: Box::new(source),
            },
        )
        .await
        .unwrap();
        let job = db::scans::find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(job.errors, 1);
        assert_eq!(job.imported, 0);
        pool.close().await;
    }
}
