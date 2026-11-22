//! One supervised metadata worker, persisted outcomes and explicit shutdown.

use crate::{
    config::MetadataConfig,
    db::{
        self,
        metadata::{MetadataError, MetadataJob},
    },
    library::{
        artwork,
        enrichment::{EntityKind, Proposal},
    },
    providers::{Transport, acoustid, musicbrainz},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::{path::PathBuf, sync::Arc};
use tokio::{sync::Mutex, task::JoinSet};
use utoipa::ToSchema;

/// One bounded operation produces candidates without modifying catalog fields.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    /// Search literal text, returning at most ten candidate identities.
    Search,
    /// Resolve an exact MusicBrainz UUID.
    Lookup,
    /// Extract one embedded image from an associated registered audio file.
    EmbeddedArtwork,
    /// Fetch CAA front art for the confirmed release MBID.
    CoverArt,
    /// Optional fpcalc + AcoustID lookup of an associated recording file.
    Fingerprint,
}

impl Action {
    /// Persist a stable action identifier in job history.
    fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Lookup => "lookup",
            Self::EmbeddedArtwork => "embedded_artwork",
            Self::CoverArt => "cover_art",
            Self::Fingerprint => "fingerprint",
        }
    }
}

/// Strict operation-specific input; unrelated options are rejected.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct JobRequest {
    /// Operation to execute.
    pub(crate) action: Action,
    /// Search text; omitted uses current name/title.
    pub(crate) query: Option<String>,
    /// Exact ID, required only for lookup.
    pub(crate) mbid: Option<String>,
    /// Registered source ID, required for embedded artwork or fingerprint.
    pub(crate) file_id: Option<i64>,
}

/// Admission failures separated from errors during accepted work.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StartError {
    /// Disabled integration, malformed options, or unsupported target.
    #[error("{0}")]
    Invalid(&'static str),
    /// One job is already active or shutdown has started.
    #[error("Metadata worker is busy or stopping")]
    Busy,
    /// Target does not exist or source is not associated with it.
    #[error(transparent)]
    Metadata(#[from] MetadataError),
    /// Unexpected storage error.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Shared service; no tasks or external requests start until admission.
#[derive(Clone)]
pub(crate) struct MetadataService {
    /// Catalog pool retained until every accepted operation finishes.
    pool: SqlitePool,
    /// Canonical roots for secure descriptor-based file access.
    roots: Arc<Vec<PathBuf>>,
    /// Operator settings, including optional secret; never logged.
    config: Arc<MetadataConfig>,
    /// Shared paced transport exists only when external calls are enabled.
    http: Option<Transport>,
    /// Admission/shutdown synchronization and owned supervisor tasks.
    workers: Arc<Mutex<Workers>>,
    /// Manual correction requests retain priority over scheduled proposals.
    priority: Arc<super::ManualPriority>,
}

/// A single admission lock prevents a job from racing with shutdown.
#[derive(Default)]
struct Workers {
    /// Once true, reject new jobs.
    stopping: bool,
    /// Every accepted worker is awaited before the pool closes.
    tasks: JoinSet<()>,
}

impl MetadataService {
    /// Create idle adapters; construction never invokes a provider or fpcalc.
    pub(crate) fn new(
        pool: SqlitePool,
        roots: Arc<Vec<PathBuf>>,
        config: MetadataConfig,
    ) -> Result<Self> {
        let http = if config.enabled {
            Some(Transport::new(&config.user_agent)?)
        } else {
            None
        };
        Ok(Self {
            pool,
            roots,
            config: Arc::new(config),
            http,
            workers: Arc::new(Mutex::new(Workers::default())),
            priority: Arc::new(super::ManualPriority::default()),
        })
    }

    /// Validate inputs and target, persist admission, then supervise the
    /// worker.
    pub(crate) async fn start(
        &self,
        kind: EntityKind,
        target: i64,
        input: JobRequest,
    ) -> Result<MetadataJob, StartError> {
        self.priority.manual();
        self.start_inner(kind, target, input, false).await
    }

    /// Scheduled work produces proposals only, with the same source
    /// validation.
    pub(crate) async fn start_automatic(
        &self,
        kind: EntityKind,
        target: i64,
        input: JobRequest,
    ) -> Result<MetadataJob, StartError> {
        self.start_inner(kind, target, input, true).await
    }

    /// Share the complete validation and admission path between both triggers.
    async fn start_inner(
        &self,
        kind: EntityKind,
        target: i64,
        mut input: JobRequest,
        automatic: bool,
    ) -> Result<MetadataJob, StartError> {
        let entity = db::metadata::get(&self.pool, kind, target).await?;
        let file_action = matches!(
            input.action,
            Action::EmbeddedArtwork | Action::Fingerprint
        );
        if file_action != input.file_id.is_some()
            || (!matches!(input.action, Action::Search)
                && input.query.is_some())
            || (matches!(input.action, Action::Lookup) != input.mbid.is_some())
        {
            return Err(StartError::Invalid(
                "Invalid options for this action",
            ));
        }
        if let Some(id) = &mut input.mbid {
            *id = crate::library::enrichment::mbid(id)
                .map_err(StartError::Invalid)?;
        }
        if matches!(input.action, Action::Search) {
            let query = input.query.get_or_insert(entity.name);
            *query = query.trim().to_owned();
            if query.is_empty()
                || query.chars().count() > 255
                || query.chars().any(char::is_control)
            {
                return Err(StartError::Invalid("Invalid search query"));
            }
        }
        if !matches!(input.action, Action::EmbeddedArtwork)
            && self.http.is_none()
        {
            return Err(StartError::Invalid("External metadata is disabled"));
        }
        if matches!(input.action, Action::Fingerprint)
            && (kind != EntityKind::Recording
                || self.config.fpcalc_path.is_empty()
                || self.config.acoustid_key.is_empty())
        {
            return Err(StartError::Invalid(
                "Fingerprint requires recording, fpcalc_path and acoustid_key",
            ));
        }
        if matches!(input.action, Action::CoverArt)
            && (kind != EntityKind::Release || entity.mbid.is_none())
        {
            return Err(StartError::Invalid(
                "Cover art requires a release with a confirmed MusicBrainz ID",
            ));
        }
        let path = if let Some(file) = input.file_id {
            let path: Option<String> = sqlx::query_scalar(match kind {
                EntityKind::Recording => {
                    "SELECT path FROM audio_files \
                    WHERE id=? AND recording_id=?"
                }
                EntityKind::Release => {
                    "SELECT f.path FROM audio_files f \
                    WHERE f.id=? AND EXISTS(SELECT 1 FROM release_tracks t \
                    WHERE t.recording_id=f.recording_id AND t.release_id=?)"
                }
                EntityKind::Artist => {
                    return Err(StartError::Invalid(
                        "Embedded art requires a recording or release",
                    ));
                }
            })
            .bind(file)
            .bind(target)
            .fetch_optional(&self.pool)
            .await?;
            Some(path.ok_or(StartError::Metadata(MetadataError::NotFound))?)
        } else {
            None
        };
        let mut workers = self.workers.lock().await;
        if automatic && !self.priority.automatic_allowed() {
            return Err(StartError::Busy);
        }
        if workers.stopping {
            return Err(StartError::Busy);
        }
        while let Some(result) = workers.tasks.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error,"Metadata supervisor failed");
            }
        }
        let result = sqlx::query_scalar::<_, i64>(
            "INSERT INTO metadata_jobs (entity_type,entity_id,action) \
            VALUES (?,?,?) RETURNING id",
        )
        .bind(kind.as_str())
        .bind(target)
        .bind(input.action.as_str())
        .fetch_one(&self.pool)
        .await;
        let id = match result {
            Err(error)
                if error
                    .as_database_error()
                    .is_some_and(|e| e.is_unique_violation()) =>
            {
                return Err(StartError::Busy);
            }
            result => result?,
        };
        let job = db::metadata::job(&self.pool, id)
            .await?
            .ok_or(StartError::Metadata(MetadataError::NotFound))?;
        let service = self.clone();
        workers.tasks.spawn(async move {
            let result = service.execute(kind, input, path, entity.mbid).await;
            let result = match result {
                Ok(proposals) => {
                    db::metadata::finish(&service.pool, id, proposals)
                        .await
                        .map_err(anyhow::Error::from)
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                tracing::error!(job_id=id,%error,"Metadata job failed");
                if let Err(error) = sqlx::query(
                    "UPDATE metadata_jobs SET status='failed', \
                    message='Metadata operation failed; see server logs', \
                    finished_at=CURRENT_TIMESTAMP WHERE id=?",
                )
                .bind(id)
                .execute(&service.pool)
                .await
                {
                    tracing::error!(job_id=id,%error,
                        "Failed to persist metadata job failure");
                }
            }
        });
        Ok(job)
    }

    /// Dispatch bounded I/O outside database transactions and request
    /// lifetime.
    async fn execute(
        &self,
        kind: EntityKind,
        input: JobRequest,
        path: Option<String>,
        confirmed: Option<String>,
    ) -> Result<Vec<(Proposal, Option<Vec<u8>>)>> {
        let source = if matches!(input.action, Action::EmbeddedArtwork) {
            "embedded"
        } else {
            "cover_art_archive"
        };
        let file = if let Some(path) = path {
            let roots = self.roots.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    let opened = crate::audio::open(
                        &roots,
                        std::path::Path::new(&path),
                    )?;
                    ensure!(
                        opened.size <= 512 * 1024 * 1024,
                        "Metadata source exceeds 512 MiB"
                    );
                    Ok::<_, anyhow::Error>(opened.file)
                })
                .await??,
            )
        } else {
            None
        };
        match input.action {
            Action::Search | Action::Lookup => {
                let http = self
                    .http
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Provider disabled"))?;
                Ok(musicbrainz::search(
                    http,
                    kind,
                    input.query.as_deref().unwrap_or(""),
                    input.mbid.as_deref(),
                )
                .await?
                .into_iter()
                .map(|p| (p, None))
                .collect())
            }
            Action::Fingerprint => {
                let http = self
                    .http
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Provider disabled"))?;
                let file = file.ok_or_else(|| anyhow::anyhow!("No source"))?;
                Ok(acoustid::identify(
                    http,
                    &self.config.fpcalc_path,
                    &self.config.acoustid_key,
                    file,
                )
                .await?
                .into_iter()
                .map(|p| (p, None))
                .collect())
            }
            Action::EmbeddedArtwork | Action::CoverArt => {
                let art = if let Some(file) = file {
                    tokio::task::spawn_blocking(move || {
                        artwork::embedded(file)
                    })
                    .await??
                } else {
                    let http = self
                        .http
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Provider disabled"))?;
                    musicbrainz::cover(
                        http,
                        confirmed
                            .as_deref()
                            .ok_or_else(|| anyhow::anyhow!("No release ID"))?,
                    )
                    .await?
                };
                Ok(vec![(
                    Proposal {
                        source: source.into(),
                        mbid: None,
                        name: None,
                        description: None,
                        artists: vec![],
                        score: None,
                        artwork_digest: Some(art.digest),
                        artwork_mime: Some(art.mime.into()),
                    },
                    Some(art.data),
                )])
            }
        }
    }

    /// Seal admission and drain every accepted worker before pool closure.
    pub(crate) async fn close(&self) {
        let mut workers = self.workers.lock().await;
        workers.stopping = true;
        while let Some(result) = workers.tasks.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error,
                    "Metadata supervisor failed at shutdown");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Real file extraction, admission recovery and proposal-only semantics.
    use super::*;

    /// Artwork jobs use configured roots, retain bytes and require acceptance.
    #[tokio::test]
    async fn extracts_embedded_art_without_mutating_catalog_or_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.wav");
        let original = artwork::Artwork::tagged_wave(&path);
        let pool = db::connect(&dir.path().join("db.sqlite")).await.unwrap();
        sqlx::query("INSERT INTO recordings (id,title) VALUES (1,'Track')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO audio_files \
            (id,recording_id,path,format) VALUES (1,1,?,'wav')",
        )
        .bind(path.to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
        let service = MetadataService::new(
            pool.clone(),
            Arc::new(vec![dir.path().to_path_buf()]),
            MetadataConfig::default(),
        )
        .unwrap();
        let input = || JobRequest {
            action: Action::EmbeddedArtwork,
            query: None,
            mbid: None,
            file_id: Some(1),
        };
        // Persisted admission also prevents a second process starting work.
        sqlx::query(
            "INSERT INTO metadata_jobs \
            (entity_type,entity_id,action) VALUES ('recording',1,'lookup')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            service.start(EntityKind::Recording, 1, input()).await,
            Err(StartError::Busy)
        ));
        db::metadata::recover(&pool).await.unwrap();
        let job = service
            .start(EntityKind::Recording, 1, input())
            .await
            .unwrap();
        service.close().await;
        let completed =
            db::metadata::job(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(completed.status, "completed");
        let candidates =
            db::metadata::candidates(&pool, job.id).await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].proposal.source, "embedded");
        assert!(
            db::metadata::artwork(&pool, EntityKind::Recording, 1)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db::metadata::get(&pool, EntityKind::Recording, 1)
                .await
                .unwrap()
                .version,
            1
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(matches!(
            service.start(EntityKind::Recording, 1, input()).await,
            Err(StartError::Busy)
        ));
        // A source outside roots is accepted as a job but fails safely later.
        let service = MetadataService::new(
            pool.clone(),
            Arc::new(vec![]),
            MetadataConfig::default(),
        )
        .unwrap();
        let job = service
            .start(EntityKind::Recording, 1, input())
            .await
            .unwrap();
        service.close().await;
        let failed = db::metadata::job(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(failed.status, "failed");
        assert!(!failed.message.unwrap().contains(path.to_str().unwrap()));
        pool.close().await;
    }
}
