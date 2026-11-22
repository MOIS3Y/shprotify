//! One supervised removal at a time, with durable restart recovery.

use crate::{db, downloads::storage::Storage};
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::{sync::Mutex, task::JoinSet};

/// Shared admission gate and owned task set, drained before SQLite closes.
#[derive(Clone)]
pub(crate) struct RemovalService(Arc<Inner>);

/// Process-local lifetime; SQLite additionally serializes durable intentions.
struct Inner {
    /// Same database as import registration.
    pool: SqlitePool,
    /// Optional exclusively owned managed storage boundary.
    storage: Option<Storage>,
    /// Admission and task registration are cancellation-safe under one lock.
    tasks: Mutex<JoinSet<()>>,
}

impl RemovalService {
    /// Create an idle executor; startup calls recovery before accepting HTTP.
    pub(crate) fn new(pool: SqlitePool, storage: Option<Storage>) -> Self {
        Self(Arc::new(Inner {
            pool,
            storage,
            tasks: Mutex::new(JoinSet::new()),
        }))
    }

    /// Resolve an interrupted operation before download recovery reuses files.
    pub(crate) async fn recover(&self) -> anyhow::Result<()> {
        if let Some(intent) = db::removals::pending(&self.0.pool).await? {
            self.execute(intent).await?;
        }
        Ok(())
    }

    /// Admit one operation, then register an owned task before releasing lock.
    /// Physical deletion requires an exact generated path before admission.
    pub(crate) async fn start(
        &self,
        recording: i64,
        file: i64,
        user: i64,
        remove_file: bool,
    ) -> Result<i64, crate::api::error::ApiError> {
        use crate::api::error::ApiError;
        let mut tasks = self.0.tasks.lock().await;
        while tasks.try_join_next().is_some() {}
        if !tasks.is_empty() {
            return Err(ApiError::Conflict("Another removal is running"));
        }
        let registered = db::audio_files::find(&self.0.pool, recording, file)
            .await?
            .ok_or(ApiError::NotFound)?;
        if remove_file {
            let storage = self.0.storage.as_ref().ok_or(
                ApiError::Conflict("Managed storage is not configured"),
            )?;
            let hash: Option<String> = sqlx::query_scalar(
                "SELECT sha256 FROM audio_file_hashes WHERE file_id=?",
            )
            .bind(file)
            .fetch_optional(&self.0.pool)
            .await?;
            let hash = hash.ok_or(ApiError::Conflict(
                "Physical removal requires a verified managed import",
            ))?;
            storage
                .checked_object(&registered.path, &hash)
                .map_err(|_| {
                    ApiError::Conflict(
                        "External collection files cannot be removed",
                    )
                })?;
        }
        // Own admission as well as execution: client disconnects cannot leave
        // a committed intent without a registered worker in this process.
        let service = self.clone();
        let (send, receive) = tokio::sync::oneshot::channel();
        tasks.spawn(async move {
            let result = db::removals::start(
                &service.0.pool,
                recording,
                file,
                user,
                remove_file,
            )
            .await;
            match result {
                Ok(Some(intent)) => {
                    let _ = send.send(Ok(intent.id));
                    if let Err(error) = service.execute(intent).await {
                        tracing::error!(%error,"File removal worker failed");
                    }
                }
                Ok(None) => {
                    let _ = send.send(Err(ApiError::Conflict(
                        "Removal is pending or file is not a managed import",
                    )));
                }
                Err(error) => {
                    let _ = send.send(Err(error.into()));
                }
            }
        });
        drop(tasks);
        receive
            .await
            .map_err(|error| ApiError::Internal(error.into()))?
    }

    /// Failed physical checks preserve registration; SQL failures keep the
    /// intent pending for restart rather than exposing partially deleted data.
    async fn execute(
        &self,
        intent: db::removals::Intent,
    ) -> anyhow::Result<()> {
        if intent.remove_file {
            let storage = self.0.storage.clone();
            let path = intent.path;
            let hash = intent.sha256;
            let result = tokio::task::spawn_blocking(move || {
                let storage = storage.ok_or_else(|| {
                    anyhow::anyhow!("Managed storage is not configured")
                })?;
                storage.remove(
                    &path,
                    &hash.ok_or_else(|| {
                        anyhow::anyhow!("Missing verified content hash")
                    })?,
                )
            })
            .await?;
            if let Err(error) = result {
                tracing::warn!(id=intent.id,%error,"File removal rejected");
                db::removals::fail(&self.0.pool, intent.id).await?;
                return Ok(());
            }
        }
        db::removals::complete(&self.0.pool, intent.id).await?;
        Ok(())
    }

    /// Wait for accepted work before downloads and the shared pool shut down.
    pub(crate) async fn close(&self) {
        let mut tasks = self.0.tasks.lock().await;
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error,"File removal task terminated");
            }
        }
    }
}
