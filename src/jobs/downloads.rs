//! Bounded durable acquisition workers with isolated child-process lifecycles.
//!
//! Queued jobs resume after restart. Unjournaled active jobs become explicitly
//! retryable interruptions; journaled imports are replayed before HTTP
//! startup.
//! Shutdown stops provider processes and retains queued work. Publication and
//! its catalog transaction finish once the import cancellation boundary wins.

use crate::{
    config::DownloadsConfig,
    db::{self, downloads::Work},
    downloads::{
        DownloadError, DownloadJob, Result, SearchRequest, storage::Storage,
    },
    library::metadata::ScannedFile,
    providers::youtube,
};
use sqlx::SqlitePool;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore, watch},
    task::JoinSet,
};

/// Application-owned acquisition service; clones share limits and shutdown.
#[derive(Clone)]
pub(crate) struct DownloadService(Arc<Inner>);

/// Shared lifecycle and immutable startup policy.
struct Inner {
    /// Catalog and persisted job queue.
    pool: SqlitePool,
    /// Validated resource limits and trusted tool paths.
    config: DownloadsConfig,
    /// Explicit managed filesystem, absent in catalog-only deployments.
    storage: Option<Storage>,
    /// Includes source searches, downloads, verification and import.
    limit: Arc<Semaphore>,
    /// Admission and task spawning cannot race graceful shutdown.
    workers: Mutex<Workers>,
    /// Cancellation signal for provider work; queued intent stays durable.
    shutdown: watch::Sender<bool>,
}

/// Owned tasks are drained before closing the database pool.
#[derive(Default)]
struct Workers {
    /// Reject new requests once application shutdown starts.
    stopping: bool,
    /// Bounded by persisted queue admission and regular finished-task
    /// draining.
    tasks: JoinSet<()>,
}

impl DownloadService {
    /// Construct without spawning tasks; call recovery before serving HTTP.
    pub(crate) fn new(
        pool: SqlitePool,
        config: DownloadsConfig,
        storage: Option<Storage>,
    ) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self(Arc::new(Inner {
            pool,
            limit: Arc::new(Semaphore::new(config.max_parallel_jobs)),
            config,
            storage,
            workers: Mutex::new(Workers::default()),
            shutdown,
        }))
    }

    /// Report configured availability; this is not a live YouTube health
    /// probe.
    pub(crate) fn enabled(&self) -> bool {
        self.0.config.enabled
            && self.0.config.youtube.enabled
            && self.0.storage.is_some()
    }

    /// Read-only policy projection for administrator configuration inspection.
    pub(crate) fn import_policy(
        &self,
    ) -> &crate::downloads::policy::ImportPolicy {
        &self.0.config.import
    }

    /// Include previously managed audio in streaming even when fetching is
    /// off.
    pub(crate) fn media_root(&self) -> Option<PathBuf> {
        self.0.storage.as_ref().map(|s| s.root.clone())
    }

    /// Finish durable publication intents, interrupt old processes, resume
    /// queue.
    pub(crate) async fn recover(&self) -> Result<()> {
        let intents: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT job_id,sha256,path FROM download_imports ORDER BY job_id",
        )
        .fetch_all(&self.0.pool)
        .await?;
        for (id, hash, path) in intents {
            if let Err(error) = self.recover_import(id, &hash, &path).await {
                if matches!(error, DownloadError::Storage(_)) {
                    return Err(error);
                }
                tracing::warn!(job_id=id, %error, "Import recovery failed");
                if self.0.storage.is_some() {
                    sqlx::query("DELETE FROM download_imports WHERE job_id=?")
                        .bind(id)
                        .execute(&self.0.pool)
                        .await?;
                    self.remove_unused_object(&path, &hash).await?;
                }
                db::downloads::finish(
                    &self.0.pool,
                    id,
                    "failed",
                    Some("Import recovery failed; check managed storage"),
                )
                .await?;
            }
        }
        let queued = db::downloads::recover(&self.0.pool).await?;
        if let Some(storage) = &self.0.storage {
            let ended: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM download_jobs WHERE status NOT IN \
                ('queued','running','verifying','importing')",
            )
            .fetch_all(&self.0.pool)
            .await?;
            for id in ended {
                if let Err(error) = storage.cleanup(id) {
                    tracing::warn!(job_id=id, %error,
                        "Cannot clean interrupted scratch space");
                }
            }
        }
        if self.enabled() {
            let mut workers = self.0.workers.lock().await;
            for id in queued {
                self.spawn(&mut workers, id);
            }
        }
        Ok(())
    }

    /// Remove only a generated object unreferenced by catalog or other
    /// journals.
    /// Recovery runs before provider workers start, so publication cannot
    /// race.
    async fn remove_unused_object(
        &self,
        path: &str,
        hash: &str,
    ) -> Result<()> {
        let Some(storage) = &self.0.storage else {
            return Ok(());
        };
        let Ok(object) = storage.checked_object(path, hash) else {
            return Ok(());
        };
        let retained: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM audio_files WHERE path=?) \
            OR EXISTS(SELECT 1 FROM download_imports WHERE path=?)",
        )
        .bind(path)
        .bind(path)
        .fetch_one(&self.0.pool)
        .await?;
        if !retained
            && let Err(error) = std::fs::remove_file(object)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "Cannot remove failed import artifact");
        }
        Ok(())
    }

    /// Replay one durable intent; corrupt/provider artifacts fail only this
    /// job.
    async fn recover_import(
        &self,
        id: i64,
        hash: &str,
        path: &str,
    ) -> Result<()> {
        let storage = self.storage()?.clone();
        let object = storage.checked_object(path, hash).map_err(|_| {
            DownloadError::Failed("Invalid import recovery path")
        })?;
        let stage = storage.stage(id).map_err(|_| {
            DownloadError::Failed("Cannot open import scratch space")
        })?;
        if !object.try_exists()? {
            if let Ok(artifact) = Storage::artifact(&stage) {
                storage.publish(&artifact, &object).map_err(|_| {
                    DownloadError::Failed("Cannot recover audio publication")
                })?;
            } else {
                sqlx::query("DELETE FROM download_imports WHERE job_id=?")
                    .bind(id)
                    .execute(&self.0.pool)
                    .await?;
                db::downloads::finish(
                    &self.0.pool,
                    id,
                    "interrupted",
                    Some("Import artifact missing; retry explicitly"),
                )
                .await?;
                storage.cleanup(id).map_err(|_| {
                    DownloadError::Failed("Cannot clean interrupted download")
                })?;
                return Ok(());
            }
        }
        let work = db::downloads::work(&self.0.pool, id).await?;
        let source = self.probe(object).await?;
        db::download_import::commit(
            &self.0.pool,
            &work,
            source,
            &self.0.config.import,
        )
        .await?;
        storage.cleanup(id).map_err(|_| {
            DownloadError::Failed("Cannot clean recovered download")
        })?;
        Ok(())
    }

    /// Queue a source search after resolving owned Discovery/shared catalog
    /// intent.
    pub(crate) async fn search(
        &self,
        user: i64,
        request: SearchRequest,
    ) -> Result<DownloadJob> {
        self.available()?;
        let target =
            db::downloads::target(&self.0.pool, user, request).await?;
        self.admit(user, &target, None, None).await
    }

    /// Accept only an owned recent search selection, revalidating current
    /// intent.
    pub(crate) async fn download(
        &self,
        user: i64,
        source: i64,
    ) -> Result<DownloadJob> {
        self.available()?;
        let (old, video, duration) =
            db::downloads::select(&self.0.pool, user, source).await?;
        let mut current =
            db::downloads::target(&self.0.pool, user, old.request.clone())
                .await?;
        if current.title != old.title
            || current.artists != old.artists
            || current.candidate.as_ref().and_then(|c| c.mbid.as_ref())
                != old.candidate.as_ref().and_then(|c| c.mbid.as_ref())
            || (old.recording_id.is_some()
                && current.recording_id != old.recording_id)
        {
            return Err(DownloadError::Conflict(
                "Target changed; search sources again",
            ));
        }
        current.source_duration_seconds = duration;
        self.admit(user, &current, Some(&video), None).await
    }

    /// Explicit retry rechecks visibility, limits and the original source
    /// choice.
    pub(crate) async fn retry(
        &self,
        user: i64,
        id: i64,
    ) -> Result<DownloadJob> {
        self.available()?;
        let job = db::downloads::job(&self.0.pool, user, id).await?;
        if !matches!(
            job.status.as_str(),
            "failed" | "cancelled" | "interrupted"
        ) {
            return Err(DownloadError::Conflict(
                "Only unsuccessful jobs can be retried",
            ));
        }
        let journaled: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM download_imports WHERE job_id=?)",
        )
        .bind(id)
        .fetch_one(&self.0.pool)
        .await?;
        if journaled {
            return Err(DownloadError::Conflict(
                "Import awaits server recovery",
            ));
        }
        let old = db::downloads::work(&self.0.pool, id).await?;
        let mut target = db::downloads::target(
            &self.0.pool,
            user,
            old.target.request.clone(),
        )
        .await?;
        if job.operation == "download"
            && (target.title != old.target.title
                || target.artists != old.target.artists
                || target.candidate.as_ref().and_then(|c| c.mbid.as_ref())
                    != old
                        .target
                        .candidate
                        .as_ref()
                        .and_then(|c| c.mbid.as_ref())
                || (old.target.recording_id.is_some()
                    && target.recording_id != old.target.recording_id))
        {
            return Err(DownloadError::Conflict(
                "Target changed; search sources again",
            ));
        }
        target.source_duration_seconds = old.target.source_duration_seconds;
        self.admit(user, &target, job.video_id.as_deref(), Some(id))
            .await
    }

    /// Seal admission, stop provider process groups and drain critical
    /// imports.
    pub(crate) async fn close(&self) {
        let mut workers = self.0.workers.lock().await;
        workers.stopping = true;
        self.0.shutdown.send_replace(true);
        while let Some(result) = workers.tasks.join_next().await {
            if let Err(error) = result {
                tracing::error!(%error, "Download worker failed unexpectedly");
            }
        }
    }

    /// Disabled provider requests fail before any durable queue mutation.
    fn available(&self) -> Result<()> {
        if !self.enabled() {
            return Err(DownloadError::Invalid(
                "YouTube downloads are disabled",
            ));
        }
        Ok(())
    }

    /// Require the explicitly configured managed root.
    fn storage(&self) -> Result<&Storage> {
        self.0.storage.as_ref().ok_or(DownloadError::Failed(
            "Configure managed storage to recover pending imports",
        ))
    }

    /// Serialize admission/spawn with shutdown so accepted work is supervised.
    async fn admit(
        &self,
        user: i64,
        target: &crate::downloads::Target,
        video: Option<&str>,
        retry: Option<i64>,
    ) -> Result<DownloadJob> {
        let mut workers = self.0.workers.lock().await;
        if workers.stopping {
            return Err(DownloadError::Conflict("Server is stopping"));
        }
        while let Some(result) = workers.tasks.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error, "Download worker failed unexpectedly");
            }
        }
        let job = db::downloads::enqueue(
            &self.0.pool,
            &self.0.config,
            user,
            target,
            video,
            retry,
        )
        .await?;
        self.spawn(&mut workers, job.id);
        Ok(job)
    }

    /// Retain every spawned task until shutdown or explicit completion
    /// draining.
    fn spawn(&self, workers: &mut Workers, id: i64) {
        let service = self.clone();
        workers.tasks.spawn(async move {
            if let Err(error) = service.run(id).await {
                tracing::warn!(job_id=id, %error, "Acquisition job failed");
                let message = match &error {
                    DownloadError::Failed(message) => *message,
                    DownloadError::Conflict(message) => *message,
                    _ => "Acquisition failed; see server logs",
                };
                if let Err(error) = db::downloads::finish(
                    &service.0.pool,
                    id,
                    "failed",
                    Some(message),
                )
                .await
                {
                    tracing::error!(job_id=id, %error,
                        "Cannot finish download job");
                }
            }
            let journaled: std::result::Result<bool, _> = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM download_imports WHERE job_id=?)",
            )
            .bind(id)
            .fetch_one(&service.0.pool)
            .await;
            if matches!(journaled, Ok(false))
                && let Some(storage) = &service.0.storage
                && let Err(error) = storage.cleanup(id)
            {
                tracing::warn!(job_id=id, %error,
                    "Cannot clean download scratch space");
            }
        });
    }

    /// Wait fairly for a slot while cancelled queued tasks can still
    /// terminate.
    async fn run(&self, id: i64) -> Result<()> {
        let mut shutdown = self.0.shutdown.subscribe();
        let acquire = self.0.limit.clone().acquire_owned();
        tokio::pin!(acquire);
        let _permit = loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            tokio::select! {
                biased;
                _ = shutdown.changed() => return Ok(()),
                permit = &mut acquire => break permit.map_err(|_|
                    DownloadError::Conflict("Server is stopping"))?,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    let work = db::downloads::work(&self.0.pool, id).await?;
                    if work.job.status != "queued" { return Ok(()); }
                }
            }
        };
        if !db::downloads::claim(&self.0.pool, id).await? {
            return Ok(());
        }
        let work = db::downloads::work(&self.0.pool, id).await?;
        tracing::info!(job_id=id, user_id=work.user_id,
            operation=%work.job.operation, "Acquisition job started");
        if work.job.operation == "download"
            && self.try_reuse(&work, None).await?
        {
            return Ok(());
        }
        if work.job.operation == "download" {
            db::download_import::check_policy(
                &self.0.pool,
                &work.target,
                &self.0.config.import,
            )
            .await?;
        }
        let stage = if work.job.operation == "download" {
            Some(self.storage()?.stage(id).map_err(|_| {
                DownloadError::Failed("Cannot prepare download scratch space")
            })?)
        } else {
            None
        };
        let seconds = if stage.is_some() {
            self.0.config.timeout_seconds
        } else {
            60
        };
        let operation = async {
            if let Some(stage) = &stage {
                youtube::download(
                    &self.0.config,
                    work.job.video_id.as_deref().ok_or(
                        DownloadError::Failed("Missing source identity"),
                    )?,
                    stage,
                )
                .await?;
                Ok(None)
            } else {
                youtube::search(&self.0.config, &work.target)
                    .await
                    .map(Some)
            }
        };
        let result = tokio::select! {
            biased;
            _ = shutdown.changed() => {
                db::downloads::finish(&self.0.pool, id, "interrupted",
                    Some("Server stopped; retry explicitly")).await?;
                return Ok(());
            }
            result = self.monitor(id, stage.as_deref()) => return result,
            result = tokio::time::timeout(
                Duration::from_secs(seconds), operation) => {
                result.map_err(|_| DownloadError::Failed(
                    "Provider operation timed out"))??
            },
        };
        if let Some(sources) = result {
            db::downloads::searched(&self.0.pool, id, &sources).await?;
            return Ok(());
        }
        sqlx::query(
            "UPDATE download_jobs SET status='verifying' \
            WHERE id=? AND status='running'",
        )
        .bind(id)
        .execute(&self.0.pool)
        .await?;
        // Blocking probes are awaited to retain the resource permit and file
        // lifetime; cancelling a spawn_blocking future would not stop its
        // work.
        self.import(
            &work,
            stage
                .as_deref()
                .ok_or(DownloadError::Failed("Missing download artifact"))?,
        )
        .await
    }

    /// Poll persisted cancellation and enforce disk bounds independent of
    /// yt-dlp.
    async fn monitor(&self, id: i64, stage: Option<&Path>) -> Result<()> {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let size = if let Some(stage) = stage {
                let stage = stage.to_owned();
                tokio::task::spawn_blocking(move || {
                    Storage::stage_size(&stage)
                })
                .await
                .map_err(|_| DownloadError::Failed("Disk monitor failed"))?
                .map_err(|_| {
                    DownloadError::Failed("Invalid download scratch contents")
                })?
            } else {
                0
            };
            // Remuxing may temporarily retain both the input and output file.
            if size > self.0.config.max_file_bytes.saturating_mul(2) {
                return Err(DownloadError::Failed(
                    "Download exceeds disk size limit",
                ));
            }
            if db::downloads::progress(&self.0.pool, id, size).await? {
                db::downloads::finish(&self.0.pool, id, "cancelled", None)
                    .await?;
                return Ok(());
            }
        }
    }

    /// Verify, journal, publish and register one artifact without touching
    /// originals.
    async fn import(&self, work: &Work, stage: &Path) -> Result<()> {
        let id = work.job.id;
        let path = Storage::artifact(stage).map_err(|_| {
            DownloadError::Failed(
                "Provider did not produce one complete audio file",
            )
        })?;
        let source = self.probe(path.clone()).await?;
        if let Some(expected) = work.target.source_duration_seconds {
            let actual = source.properties.duration_ms.unwrap_or(0);
            let expected = expected.saturating_mul(1000);
            if (actual - expected).abs() > (expected / 20).max(10_000) {
                return Err(DownloadError::Failed(
                    "Audio duration differs from source; search again",
                ));
            }
        }
        db::downloads::progress(
            &self.0.pool,
            id,
            source.properties.size_bytes.unwrap_or(0) as u64,
        )
        .await?;
        let hash = source
            .sha256
            .as_deref()
            .ok_or(DownloadError::Failed("Missing audio content hash"))?;
        if self.try_reuse(work, Some(hash)).await? {
            return Ok(());
        }
        if *self.0.shutdown.borrow() {
            db::downloads::finish(
                &self.0.pool,
                id,
                "interrupted",
                Some("Server stopped; retry explicitly"),
            )
            .await?;
            return Ok(());
        }
        db::download_import::check_policy(
            &self.0.pool,
            &work.target,
            &self.0.config.import,
        )
        .await?;
        let storage = self.storage()?;
        let object = storage.object(hash, &source.format);
        let name = object
            .to_str()
            .ok_or(DownloadError::Failed("Invalid managed path"))?;
        db::download_import::journal(&self.0.pool, id, hash, name).await?;
        let publisher = storage.clone();
        let destination = object.clone();
        tokio::task::spawn_blocking(move || {
            publisher.publish(&path, &destination)
        })
        .await
        .map_err(|_| DownloadError::Failed("Publication worker failed"))?
        .map_err(|_| {
            DownloadError::Failed("Cannot publish downloaded audio")
        })?;
        let final_source = self.probe(object).await?;
        db::download_import::commit(
            &self.0.pool,
            work,
            final_source,
            &self.0.config.import,
        )
        .await
    }

    /// Reuse a readable exact source/hash without trusting stale registry
    /// data.
    async fn try_reuse(
        &self,
        work: &Work,
        hash: Option<&str>,
    ) -> Result<bool> {
        let video = work
            .job
            .video_id
            .as_deref()
            .ok_or(DownloadError::Failed("Missing source identity"))?;
        let Some((recording, file)) =
            db::download_import::existing(&self.0.pool, video, hash).await?
        else {
            return Ok(false);
        };
        if self.0.config.import.duplicates
            == crate::downloads::policy::DuplicatePolicy::Reject
        {
            return Err(DownloadError::Conflict(
                "Import policy rejects duplicates",
            ));
        }
        let existing = db::audio_files::find(&self.0.pool, recording, file)
            .await?
            .ok_or(DownloadError::NotFound)?;
        let actual = self.probe(PathBuf::from(existing.path)).await?;
        let registered: Option<String> = sqlx::query_scalar(
            "SELECT sha256 FROM audio_file_hashes WHERE file_id=?",
        )
        .bind(file)
        .fetch_optional(&self.0.pool)
        .await?;
        let expected = hash.or(registered.as_deref());
        if expected.is_none() || actual.sha256.as_deref() != expected {
            return Err(DownloadError::Failed(
                "Registered duplicate is unavailable or changed",
            ));
        }
        db::download_import::reuse(
            &self.0.pool,
            work,
            recording,
            file,
            &self.0.config.import,
        )
        .await?;
        Ok(true)
    }

    /// Decode/probe and hash on a blocking worker with explicit physical
    /// bounds.
    async fn probe(&self, path: PathBuf) -> Result<ScannedFile> {
        let max_size = self.0.config.max_file_bytes;
        let duration = i64::from(self.0.config.max_duration_seconds) * 1000;
        let storage = self.storage()?;
        let mut roots = storage.external.clone();
        roots.push(storage.root.clone());
        tokio::task::spawn_blocking(move || {
            let opened = crate::audio::open(&roots, &path).map_err(|_| {
                DownloadError::Failed(
                    "Audio artifact is outside allowed storage",
                )
            })?;
            let metadata = opened.file.metadata()?;
            if !metadata.is_file()
                || metadata.len() == 0
                || metadata.len() > max_size
            {
                return Err(DownloadError::Failed(
                    "Invalid audio file size or type",
                ));
            }
            let source =
                crate::library::metadata::read_opened(&path, opened.file)
                    .map_err(|_| {
                        DownloadError::Failed(
                            "Downloaded audio cannot be parsed",
                        )
                    })?
                    .ok_or(DownloadError::Failed(
                        "Provider output is not supported audio",
                    ))?;
            if source.properties.duration_ms.is_none_or(|d| d > duration)
                || source.properties.sample_rate_hz.is_none()
                || source.properties.channels.is_none()
            {
                return Err(DownloadError::Failed(
                    "Invalid audio properties or duration",
                ));
            }
            Ok(source)
        })
        .await
        .map_err(|_| {
            DownloadError::Failed("Audio verification worker failed")
        })?
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Real child-process fixtures exercise the entire offline acquisition
    //! path.
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    /// Locate tools from PATH so fixtures work in a Nix build sandbox too.
    fn executable(name: &str) -> PathBuf {
        std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|p| p.join(name))
            .find(|p| p.is_file())
            .unwrap()
    }

    /// Quote fixture paths as literal POSIX shell arguments.
    fn quote(path: &Path) -> String {
        format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
    }

    /// Persistent database, managed storage and a controllable external
    /// process.
    pub(crate) struct Fixture {
        /// Own all filesystem artifacts through service shutdown.
        pub(crate) dir: tempfile::TempDir,
        /// Migrated database with two independent member accounts.
        pub(crate) pool: SqlitePool,
        /// Server limits can be adjusted before constructing a fresh service.
        pub(crate) config: DownloadsConfig,
        /// Exclusive prepared managed root.
        pub(crate) storage: Storage,
        /// Active service with the same production worker implementation.
        pub(crate) service: DownloadService,
    }

    impl Fixture {
        /// Build a fake provider without network, cookies, FFmpeg or Deno.
        pub(crate) async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let fixture = root.join("source audio.wav");
            fs::write(&fixture, crate::library::metadata::tests::wave())
                .unwrap();
            fs::write(root.join("mode"), "ok").unwrap();
            let script = root.join("fake yt-dlp");
            let shell = executable("sh");
            let cp = quote(&executable("cp"));
            let cat = quote(&executable("cat"));
            let sleep = quote(&executable("sleep"));
            let script_text = format!(
                r#"#!{shell}
for arg do
    if [ "$arg" = '--dump-single-json' ]; then
        printf '%s\n' '{{"entries":[{{"id":"abcdefghijk",
"title":"Video title","channel":"Artist","duration":1}}]}}'
        exit 0
    fi
done
case "$({cat} {mode})" in
    fail) printf '%s\n' 'password=DO_NOT_LEAK' >&2; exit 1 ;;
    corrupt) printf 'not audio' > audio.wav ;;
    extra) {cp} {fixture} audio.wav; printf junk > extra ;;
    slow) printf part > audio.part
        {sleep} 30 &
        printf '%s\n' "$!" > {pid}
        wait ;;
    *) {cp} {fixture} audio.wav ;;
esac
"#,
                shell = shell.display(),
                fixture = quote(&fixture),
                mode = quote(&root.join("mode")),
                pid = quote(&root.join("child.pid"))
            );
            fs::write(&script, script_text).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700))
                .unwrap();
            let pool = db::connect(&root.join("catalog.db")).await.unwrap();
            sqlx::query(
                "INSERT INTO users (id,name) VALUES (1,'alice'),(2,'bob')",
            )
            .execute(&pool)
            .await
            .unwrap();
            let config = DownloadsConfig {
                enabled: true,
                directory: root.join("managed"),
                request_interval_seconds: 1,
                max_parallel_jobs: 1,
                ..DownloadsConfig::default()
            };
            let mut config = config;
            config.youtube.enabled = true;
            config.youtube.yt_dlp_path = script.to_str().unwrap().into();
            let storage = Storage::prepare(&config.directory, &[]).unwrap();
            let service = DownloadService::new(
                pool.clone(),
                config.clone(),
                Some(storage.clone()),
            );
            service.recover().await.unwrap();
            Self {
                dir,
                pool,
                config,
                storage,
                service,
            }
        }

        /// Stable desired metadata is intentionally different from source
        /// tags.
        pub(crate) fn query() -> SearchRequest {
            SearchRequest::Query {
                title: "Chosen title".into(),
                artists: vec!["Chosen artist".into()],
            }
        }

        /// Change only the fake external executable's behavior.
        pub(crate) fn mode(&self, mode: &str) {
            fs::write(self.dir.path().join("mode"), mode).unwrap();
        }

        /// Permit another search without waiting for the configured cooldown.
        pub(crate) async fn cool(&self) {
            sqlx::query("UPDATE download_jobs SET created_at=unixepoch()-10")
                .execute(&self.pool)
                .await
                .unwrap();
        }

        /// Wait for a terminal state with a bounded polling deadline.
        pub(crate) async fn wait(&self, user: i64, id: i64) -> DownloadJob {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let job = db::downloads::job(&self.pool, user, id)
                        .await
                        .unwrap();
                    if matches!(
                        job.status.as_str(),
                        "completed" | "failed" | "cancelled" | "interrupted"
                    ) {
                        return job;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("Job must reach a terminal state")
        }

        /// Resolve a real owned selection before submitting a download.
        pub(crate) async fn source(&self) -> i64 {
            let job = self.service.search(1, Self::query()).await.unwrap();
            assert_eq!(self.wait(1, job.id).await.status, "completed");
            db::downloads::sources(&self.pool, 1, job.id).await.unwrap()[0].id
        }

        /// Read the child PID only after the fake provider has started it.
        async fn child(&self) -> i32 {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(text) =
                        fs::read_to_string(self.dir.path().join("child.pid"))
                        && let Ok(pid) = text.trim().parse()
                    {
                        return pid;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap()
        }
    }

    /// Selection imports desired metadata and real properties; retries reuse
    /// bytes.
    #[tokio::test]
    async fn imports_and_reuses_selected_audio_without_rewriting_source_tags()
    {
        let f = Fixture::new().await;
        let source = f.source().await;
        let job = f.service.download(1, source).await.unwrap();
        let job = f.wait(1, job.id).await;
        assert_eq!(job.status, "completed", "{:?}", job.message);
        let recording =
            db::recordings::find(&f.pool, job.recording_id.unwrap())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(recording.title, "Chosen title");
        let file =
            db::audio_files::find(&f.pool, recording.id, job.file_id.unwrap())
                .await
                .unwrap()
                .unwrap();
        assert!(Path::new(&file.path).starts_with(&f.storage.root));
        assert_eq!(file.properties.duration_ms, Some(1000));
        let tags = db::indexing::tags(&f.pool, recording.id, file.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tags.title.as_deref(), Some("Source title"));
        assert_eq!(
            fs::read(&file.path).unwrap(),
            fs::read(f.dir.path().join("source audio.wav")).unwrap()
        );
        let search_id: i64 = sqlx::query_scalar(
            "SELECT job_id FROM download_sources WHERE id=?",
        )
        .bind(source)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        let second_source: i64 = sqlx::query_scalar(
            "INSERT INTO download_sources (job_id,video_id,title) \
            VALUES (?,'01234567890','Different upload') RETURNING id",
        )
        .bind(search_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        let same_bytes = f.service.download(1, second_source).await.unwrap();
        assert_eq!(f.wait(1, same_bytes.id).await.file_id, Some(file.id));
        f.mode("fail");
        let repeated = f.service.download(1, source).await.unwrap();
        let repeated = f.wait(1, repeated.id).await;
        assert_eq!(repeated.status, "completed");
        assert_eq!(repeated.file_id, Some(file.id));
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audio_files")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        f.service.close().await;
        assert_eq!(
            fs::read_dir(f.storage.root.join(".staging"))
                .unwrap()
                .count(),
            0
        );
        f.pool.close().await;
    }

    /// Provider failures and corrupt files stay isolated; manual retry is
    /// explicit.
    #[tokio::test]
    async fn isolates_errors_and_retries_without_registering_invalid_files() {
        let f = Fixture::new().await;
        let source = f.source().await;
        for mode in ["fail", "corrupt", "extra"] {
            f.mode(mode);
            let job = f.service.download(1, source).await.unwrap();
            let failed = f.wait(1, job.id).await;
            assert_eq!(failed.status, "failed");
            assert!(
                !serde_json::to_string(&failed)
                    .unwrap()
                    .contains("DO_NOT_LEAK")
            );
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM recordings")
                    .fetch_one(&f.pool)
                    .await
                    .unwrap();
            assert_eq!(count, 0);
        }
        f.mode("ok");
        let jobs = db::downloads::jobs(&f.pool, 1, 1, 0).await.unwrap();
        let job = f.service.retry(1, jobs[0].id).await.unwrap();
        assert_eq!(job.retry_of, Some(jobs[0].id));
        assert_eq!(f.wait(1, job.id).await.status, "completed");
        assert!(matches!(
            f.service.retry(2, job.id).await,
            Err(DownloadError::NotFound)
        ));
        assert!(f.service.retry(1, job.id).await.is_err());
        f.service.close().await;
        f.pool.close().await;
    }

    /// Cancel and shutdown stop descendant processes; pending jobs remain
    /// durable.
    #[tokio::test]
    async fn cancellation_kills_process_groups_and_shutdown_preserves_queue() {
        let f = Fixture::new().await;
        let source = f.source().await;
        f.mode("slow");
        let job = f.service.download(1, source).await.unwrap();
        let pid = f.child().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let active = db::downloads::job(&f.pool, 1, job.id).await.unwrap();
        assert!(active.downloaded_bytes > 0);
        db::downloads::cancel(&f.pool, 1, job.id).await.unwrap();
        assert_eq!(f.wait(1, job.id).await.status, "cancelled");
        assert_process_stopped(pid).await;
        fs::remove_file(f.dir.path().join("child.pid")).unwrap();
        let job = f.service.retry(1, job.id).await.unwrap();
        let pid = f.child().await;
        f.cool().await;
        let pending = f
            .service
            .search(
                1,
                SearchRequest::Query {
                    title: "Other".into(),
                    artists: vec![],
                },
            )
            .await
            .unwrap();
        f.service.close().await;
        assert_process_stopped(pid).await;
        assert_eq!(
            db::downloads::job(&f.pool, 1, job.id).await.unwrap().status,
            "interrupted"
        );
        assert_eq!(
            db::downloads::job(&f.pool, 1, pending.id)
                .await
                .unwrap()
                .status,
            "queued"
        );
        f.mode("ok");
        let restarted = DownloadService::new(
            f.pool.clone(),
            f.config.clone(),
            Some(f.storage.clone()),
        );
        restarted.recover().await.unwrap();
        assert_eq!(f.wait(1, pending.id).await.status, "completed");
        restarted.close().await;
        f.pool.close().await;
    }

    /// A killed descendant may briefly remain a zombie awaiting its own
    /// parent.
    async fn assert_process_stopped(pid: i32) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
                if stat
                    .as_ref()
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    || stat.as_ref().is_ok_and(|s| {
                        s.rsplit_once(')').is_some_and(|(_, tail)| {
                            tail.trim_start().starts_with('Z')
                        })
                    })
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Provider descendants must stop");
    }

    /// A runtime deadline stops the isolated process without user
    /// cancellation.
    #[tokio::test]
    async fn times_out_unresponsive_provider_without_affecting_catalog() {
        let f = Fixture::new().await;
        let source = f.source().await;
        f.service.close().await;
        f.mode("slow");
        let mut config = f.config.clone();
        config.timeout_seconds = 1;
        let service = DownloadService::new(
            f.pool.clone(),
            config,
            Some(f.storage.clone()),
        );
        let job = service.download(1, source).await.unwrap();
        let pid = f.child().await;
        let failed = f.wait(1, job.id).await;
        assert_eq!(failed.status, "failed");
        assert_eq!(
            failed.message.as_deref(),
            Some("Provider operation timed out")
        );
        assert_process_stopped(pid).await;
        service.close().await;
        f.pool.close().await;
    }

    /// Crash replay covers publication before/after linking and is idempotent.
    #[tokio::test]
    async fn recovers_journaled_import_before_publication_idempotently() {
        let f = Fixture::new().await;
        f.service.close().await;
        let target = db::downloads::target(&f.pool, 1, Fixture::query())
            .await
            .unwrap();
        let job = db::downloads::enqueue(
            &f.pool,
            &f.config,
            1,
            &target,
            Some("abcdefghijk"),
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE download_jobs SET status='verifying' WHERE id=?")
            .bind(job.id)
            .execute(&f.pool)
            .await
            .unwrap();
        let stage = f.storage.stage(job.id).unwrap();
        let path = stage.join("audio.wav");
        fs::write(&path, crate::library::metadata::tests::wave()).unwrap();
        let source = crate::library::metadata::read(&path).unwrap().unwrap();
        let hash = source.sha256.as_deref().unwrap();
        let object = f.storage.object(hash, &source.format);
        db::download_import::journal(
            &f.pool,
            job.id,
            hash,
            object.to_str().unwrap(),
        )
        .await
        .unwrap();
        // Crash before publication: recovery publishes the retained staging
        // link.
        let service = DownloadService::new(
            f.pool.clone(),
            f.config.clone(),
            Some(f.storage.clone()),
        );
        service.recover().await.unwrap();
        let done = db::downloads::job(&f.pool, 1, job.id).await.unwrap();
        assert_eq!(done.status, "completed");
        service.recover().await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audio_files")
                .fetch_one(&f.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        service.close().await;
        f.pool.close().await;
    }
}

#[cfg(test)]
mod failure_tests {
    //! Hard failure bounds and recovery never depend on a healthy provider.
    use super::tests::Fixture;
    use super::*;

    /// Encoded-size verification and missing tools cannot create catalog rows.
    #[tokio::test]
    async fn rejects_oversized_audio_and_missing_executables() {
        let f = Fixture::new().await;
        let source = f.source().await;
        f.service.close().await;
        for missing in [false, true] {
            let mut config = f.config.clone();
            if missing {
                config.youtube.yt_dlp_path =
                    "/nonexistent/shprotify-yt-dlp".into();
            } else {
                config.max_file_bytes = 1000;
            }
            let service = DownloadService::new(
                f.pool.clone(),
                config,
                Some(f.storage.clone()),
            );
            let job = service.download(1, source).await.unwrap();
            let failed = f.wait(1, job.id).await;
            assert_eq!(failed.status, "failed");
            assert_eq!(failed.file_id, None);
            let count: i64 =
                sqlx::query_scalar("SELECT count(*) FROM recordings")
                    .fetch_one(&f.pool)
                    .await
                    .unwrap();
            assert_eq!(count, 0);
            service.close().await;
        }
        f.pool.close().await;
    }

    /// Corrupt journaled audio fails only its job, leaving startup
    /// operational.
    #[tokio::test]
    async fn invalid_import_recovery_does_not_break_local_service() {
        let f = Fixture::new().await;
        f.service.close().await;
        let target = db::downloads::target(&f.pool, 1, Fixture::query())
            .await
            .unwrap();
        let job = db::downloads::enqueue(
            &f.pool,
            &f.config,
            1,
            &target,
            Some("abcdefghijk"),
            None,
        )
        .await
        .unwrap();
        sqlx::query("UPDATE download_jobs SET status='verifying' WHERE id=?")
            .bind(job.id)
            .execute(&f.pool)
            .await
            .unwrap();
        let hash = "a".repeat(64);
        let object = f.storage.object(&hash, "wav");
        db::download_import::journal(
            &f.pool,
            job.id,
            &hash,
            object.to_str().unwrap(),
        )
        .await
        .unwrap();
        std::fs::write(&object, b"invalid audio").unwrap();
        let service = DownloadService::new(
            f.pool.clone(),
            f.config.clone(),
            Some(f.storage.clone()),
        );
        service.recover().await.unwrap();
        let failed = db::downloads::job(&f.pool, 1, job.id).await.unwrap();
        assert_eq!(failed.status, "failed");
        assert!(!object.exists());
        let resumed = service.retry(1, job.id).await.unwrap();
        assert_eq!(f.wait(1, resumed.id).await.status, "completed");
        service.close().await;
        f.pool.close().await;
    }
}
