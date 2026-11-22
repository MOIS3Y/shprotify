//! A small interval dispatcher reusing the existing supervised job services.
//!
//! SQLite retains next attempts and attached worker IDs. No HTTP self-calls,
//! cron expressions, missed-run replay or external queue are required. Native
//! workers own network/filesystem work and retain their existing limits.

use crate::{
    automation::{AutomationError, Result, Target},
    config::AutomationConfig,
    db::{self, automation::Work},
    jobs::{
        ScanService,
        discovery::DiscoveryService,
        metadata::{Action, JobRequest, MetadataService},
    },
};
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::{sync::watch, task::JoinHandle};

/// Dispatch policy and already supervised executors; no extra provider
/// clients.
#[derive(Clone)]
pub(crate) struct AutomationService {
    /// Same database used by native jobs and HTTP ownership checks.
    pool: SqlitePool,
    /// Immutable startup intervals, feature switches and schedule limits.
    config: AutomationConfig,
    /// Incremental external-library scanner.
    scans: ScanService,
    /// Private feed refresher, retaining owner cooldown/provider pacing.
    discovery: DiscoveryService,
    /// Produces proposals for explicit targets; never applies corrections.
    metadata: MetadataService,
}

/// The application stops dispatch before draining native worker services.
pub(crate) struct AutomationWorker {
    /// Shared stop signal also used when HTTP receives SIGINT/SIGTERM.
    stop: watch::Sender<bool>,
    /// Owned dispatcher task; no work survives database shutdown.
    task: JoinHandle<()>,
}

impl AutomationWorker {
    /// Let HTTP shutdown seal future automatic admissions immediately.
    pub(crate) fn stop_signal(&self) -> watch::Sender<bool> {
        self.stop.clone()
    }

    /// Stop and await dispatch; native workers are drained separately
    /// afterward.
    pub(crate) async fn close(self) {
        self.stop.send_replace(true);
        if let Err(error) = self.task.await {
            tracing::error!(%error,
                "Automation dispatcher stopped unexpectedly");
        }
    }
}

impl AutomationService {
    /// Construct an idle dispatcher without starting jobs or periodic tasks.
    pub(crate) fn new(
        pool: SqlitePool,
        config: AutomationConfig,
        scans: ScanService,
        discovery: DiscoveryService,
        metadata: MetadataService,
    ) -> Self {
        Self {
            pool,
            config,
            scans,
            discovery,
            metadata,
        }
    }

    /// Run after native job recovery; first automatic dispatch waits thirty
    /// secs.
    pub(crate) async fn start(self) -> Result<AutomationWorker> {
        db::automation::initialize(&self.pool, &self.config).await?;
        self.poll().await?;
        let (stop, mut receiver) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_secs(30));
            timer.set_missed_tick_behavior(
                tokio::time::MissedTickBehavior::Skip,
            );
            timer.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = receiver.changed() => break,
                    _ = timer.tick() => {
                        if let Err(error) = self.tick(&receiver).await {
                            tracing::error!(%error, "Automation tick failed");
                        }
                    }
                }
            }
        });
        Ok(AutomationWorker { stop, task })
    }

    /// Reconcile bounded terminal outcomes before admitting a fair due batch.
    async fn tick(&self, stop: &watch::Receiver<bool>) -> Result<()> {
        self.poll().await?;
        if !self.config.enabled {
            return Ok(());
        }
        for id in db::automation::due(&self.pool).await? {
            if *stop.borrow() {
                break;
            }
            let Some(work) =
                db::automation::claim(&self.pool, &self.config, id).await?
            else {
                continue;
            };
            if let Err(error) = self.dispatch(&work).await {
                tracing::error!(schedule_id=work.id, %error,
                    "Automatic dispatch failed");
                db::automation::settle(
                    &self.pool,
                    &self.config,
                    work.id,
                    "failed",
                    Some("Automatic admission failed; see server logs"),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Retain a successful feed after failure and compute backoff on
    /// completion.
    async fn poll(&self) -> Result<()> {
        for work in db::automation::running(&self.pool).await? {
            let status = db::automation::outcome(&self.pool, &work).await?;
            match status.as_str() {
                "running" => {}
                "completed" => {
                    db::automation::settle(
                        &self.pool,
                        &self.config,
                        work.id,
                        "completed",
                        None,
                    )
                    .await?
                }
                _ => {
                    db::automation::settle(
                        &self.pool,
                        &self.config,
                        work.id,
                        "failed",
                        Some("Automatic job failed or was interrupted"),
                    )
                    .await?
                }
            }
        }
        Ok(())
    }

    /// Expected worker contention is a short deferral, never a terminal
    /// failure.
    async fn dispatch(&self, work: &Work) -> Result<()> {
        let result = match db::automation::target(work)? {
            Target::Library => {
                if self.scans.roots().is_empty() {
                    return self
                        .pause(work.id, "No external library roots configured")
                        .await;
                }
                match self.scans.start_automatic().await {
                    Ok(job) => Some(job.id),
                    Err(
                        super::StartError::Busy | super::StartError::Stopping,
                    ) => None,
                    Err(super::StartError::Storage(error)) => {
                        return Err(error.into());
                    }
                }
            }
            Target::Discovery(feed) => {
                let user = work.owner_id.ok_or(AutomationError::NotFound)?;
                match self.discovery.refresh_automatic(user, feed).await {
                    Ok(job) => Some(job.id),
                    Err(db::discovery::DiscoveryError::Conflict) => None,
                    Err(db::discovery::DiscoveryError::Storage(error)) => {
                        return Err(error.into());
                    }
                    Err(_) => return self
                        .pause(
                            work.id,
                            "Feed target or required provider is unavailable",
                        )
                        .await,
                }
            }
            Target::Metadata(kind, id) => {
                let pending: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM metadata_candidates c \
                    JOIN metadata_jobs j ON j.id=c.job_id \
                    WHERE j.entity_type=? AND j.entity_id=? \
                        AND c.state='pending')",
                )
                .bind(kind.as_str())
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
                if pending {
                    return self
                        .pause(
                            work.id,
                            "Existing metadata proposals need manual review",
                        )
                        .await;
                }
                let entity = match db::metadata::get(&self.pool, kind, id)
                    .await
                {
                    Ok(entity) => entity,
                    Err(db::metadata::MetadataError::Storage(error)) => {
                        return Err(error.into());
                    }
                    Err(_) => {
                        return self
                            .pause(work.id, "Metadata target is unavailable")
                            .await;
                    }
                };
                let input = JobRequest {
                    action: if entity.mbid.is_some() {
                        Action::Lookup
                    } else {
                        Action::Search
                    },
                    query: None,
                    mbid: entity.mbid,
                    file_id: None,
                };
                match self.metadata.start_automatic(kind, id, input).await {
                    Ok(job) => Some(job.id),
                    Err(super::metadata::StartError::Busy) => None,
                    Err(super::metadata::StartError::Storage(error)) => {
                        return Err(error.into());
                    }
                    Err(super::metadata::StartError::Metadata(
                        db::metadata::MetadataError::Storage(error),
                    )) => {
                        return Err(error.into());
                    }
                    Err(_) => {
                        return self
                            .pause(
                                work.id,
                                "Metadata provider or target is unavailable",
                            )
                            .await;
                    }
                }
            }
        };
        if let Some(job) = result {
            db::automation::attach(&self.pool, work.id, job).await
        } else {
            db::automation::settle(
                &self.pool,
                &self.config,
                work.id,
                "deferred",
                Some("Worker busy or a manual request has priority"),
            )
            .await
        }
    }

    /// Paused targets retry infrequently without increasing failure backoff.
    async fn pause(&self, id: i64, message: &'static str) -> Result<()> {
        db::automation::settle(
            &self.pool,
            &self.config,
            id,
            "paused",
            Some(message),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    //! Real offline native jobs, restart recovery and manual admission
    //! priority.
    use super::*;
    use crate::{
        db::automation::tests::{config, feed, overdue, update},
        discovery::FeedRequest,
        library::enrichment::EntityKind,
    };

    /// Share the same native services that production HTTP handlers use.
    fn service(
        pool: &SqlitePool,
        config: AutomationConfig,
        roots: Vec<std::path::PathBuf>,
    ) -> AutomationService {
        AutomationService::new(
            pool.clone(),
            config,
            ScanService::new(pool.clone(), roots),
            DiscoveryService::new(
                pool.clone(),
                crate::config::DiscoveryConfig {
                    refresh_interval_seconds: 1,
                    ..crate::config::DiscoveryConfig::default()
                },
            )
            .unwrap(),
            MetadataService::new(
                pool.clone(),
                std::sync::Arc::new(vec![]),
                crate::config::MetadataConfig::default(),
            )
            .unwrap(),
        )
    }

    /// Drive one scheduling tick without sleeping for the production interval.
    async fn tick(service: &AutomationService) {
        let (_sender, stop) = watch::channel(false);
        service.tick(&stop).await.unwrap();
    }

    /// Await terminal native outcomes, retaining explicit scheduler polling.
    async fn complete(service: &AutomationService) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let active =
                    db::automation::running(&service.pool).await.unwrap();
                if active.is_empty() {
                    return;
                }
                service.poll().await.unwrap();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    /// Real incremental scans and private feed generation stay outside the
    /// timer.
    #[tokio::test]
    async fn runs_native_jobs_once_and_does_not_replay_missed_intervals() {
        let (dir, pool) = db::discovery::tests::setup().await;
        let root = dir.path().join("music");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("source.wav");
        let original = crate::library::metadata::tests::wave();
        std::fs::write(&path, &original).unwrap();
        let id = feed(&pool, 1, FeedRequest::Personal {}).await;
        let mut config = config();
        config.library.scan_interval_seconds = 60;
        let service = service(&pool, config.clone(), vec![root]);
        db::automation::initialize(&pool, &config).await.unwrap();
        db::automation::set(
            &pool,
            &config,
            1,
            Target::Discovery(id),
            update(0, true, 60),
        )
        .await
        .unwrap();
        overdue(&pool).await;
        tick(&service).await;
        complete(&service).await;
        let scheduled =
            db::automation::get(&pool, &config, 1, Target::Discovery(id))
                .await
                .unwrap();
        let scan = db::automation::get(&pool, &config, 0, Target::Library)
            .await
            .unwrap();
        assert_eq!(scheduled.last_status, "completed");
        assert_eq!(scan.last_status, "completed");
        assert_eq!(
            db::discovery::feed(&pool, 1, id).await.unwrap().version,
            2
        );
        let (scans, refreshes): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM library_scans), \
                (SELECT count(*) FROM discovery_jobs)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((scans, refreshes), (1, 2));
        tick(&service).await;
        assert_eq!(
            db::discovery::feed(&pool, 1, id).await.unwrap().version,
            2
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        service.scans.close().await;
        service.discovery.close().await;
        service.metadata.close().await;
        pool.close().await;
    }

    /// A completed manual request still gets its thirty-second priority
    /// window.
    #[tokio::test]
    async fn defers_to_manual_work_and_disabling_preserves_native_outcome() {
        let (_dir, pool) = db::discovery::tests::setup().await;
        let id = feed(&pool, 1, FeedRequest::Personal {}).await;
        let config = config();
        let service = service(&pool, config.clone(), vec![]);
        db::automation::set(
            &pool,
            &config,
            1,
            Target::Discovery(id),
            update(0, true, 60),
        )
        .await
        .unwrap();
        let manual = service.discovery.refresh(1, id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while db::discovery::job(&pool, 1, manual.id)
                .await
                .unwrap()
                .status
                == "running"
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        overdue(&pool).await;
        tick(&service).await;
        let schedule =
            db::automation::get(&pool, &config, 1, Target::Discovery(id))
                .await
                .unwrap();
        assert_eq!(schedule.last_status, "deferred");
        assert_eq!(schedule.failures, 0);
        assert_eq!(
            db::discovery::jobs(&pool, 1, 50, 0).await.unwrap().len(),
            2
        );
        db::automation::set(
            &pool,
            &config,
            1,
            Target::Discovery(id),
            update(1, false, 60),
        )
        .await
        .unwrap();
        tick(&service).await;
        assert_eq!(
            db::automation::get(&pool, &config, 1, Target::Discovery(id))
                .await
                .unwrap()
                .next_run_at,
            None
        );
        service.discovery.close().await;
        pool.close().await;
    }

    /// Failed native work backs off while global disable still reconciles
    /// status.
    #[tokio::test]
    async fn recovers_failed_dispatch_and_respects_global_disable() {
        let (_dir, pool) = db::discovery::tests::setup().await;
        let id = feed(&pool, 1, FeedRequest::Personal {}).await;
        let mut config = config();
        db::automation::set(
            &pool,
            &config,
            1,
            Target::Discovery(id),
            update(0, true, 60),
        )
        .await
        .unwrap();
        overdue(&pool).await;
        let schedule = db::automation::due(&pool).await.unwrap()[0];
        db::automation::claim(&pool, &config, schedule)
            .await
            .unwrap();
        db::automation::initialize(&pool, &config).await.unwrap();
        let after =
            db::automation::get(&pool, &config, 1, Target::Discovery(id))
                .await
                .unwrap();
        assert_eq!(after.last_status, "failed");
        assert_eq!(after.failures, 1);
        overdue(&pool).await;
        db::automation::claim(&pool, &config, schedule)
            .await
            .unwrap();
        let native =
            db::discovery::admit(&pool, 1, &FeedRequest::Personal {}, 0)
                .await
                .unwrap();
        db::automation::attach(&pool, schedule, native.id)
            .await
            .unwrap();
        db::discovery::recover(&pool).await.unwrap();
        config.enabled = false;
        let service = service(&pool, config.clone(), vec![]);
        tick(&service).await;
        let after =
            db::automation::get(&pool, &config, 1, Target::Discovery(id))
                .await
                .unwrap();
        assert_eq!(after.last_status, "failed");
        assert_eq!(after.failures, 2);
        assert!(after.paused_reason.is_some());
        assert_eq!(
            db::discovery::feed(&pool, 1, id).await.unwrap().version,
            1
        );
        let worker = service.start().await.unwrap();
        worker.close().await;
        pool.close().await;
    }

    /// Pending proposals suppress duplicate searches; metadata is never
    /// applied.
    #[tokio::test]
    async fn metadata_requires_live_admin_and_review_before_another_lookup() {
        let (_dir, pool) = db::discovery::tests::setup().await;
        sqlx::query("UPDATE users SET role='admin' WHERE id=1")
            .execute(&pool)
            .await
            .unwrap();
        let mut config = config();
        config.metadata.enabled = true;
        let target = Target::Metadata(EntityKind::Recording, 1);
        db::automation::set(&pool, &config, 1, target, update(0, true, 86400))
            .await
            .unwrap();
        let native: i64 = sqlx::query_scalar(
            "INSERT INTO metadata_jobs (entity_type,entity_id,action,status) \
            VALUES ('recording',1,'lookup','completed') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO metadata_candidates (job_id,proposal) \
            VALUES (?,'{}')",
        )
        .bind(native)
        .execute(&pool)
        .await
        .unwrap();
        let service = service(&pool, config.clone(), vec![]);
        overdue(&pool).await;
        tick(&service).await;
        let schedule = db::automation::get(&pool, &config, 1, target)
            .await
            .unwrap();
        assert_eq!(schedule.last_status, "paused");
        assert!(schedule.message.unwrap().contains("manual review"));
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM metadata_jobs")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        sqlx::query("UPDATE users SET role='member' WHERE id=1")
            .execute(&pool)
            .await
            .unwrap();
        overdue(&pool).await;
        tick(&service).await;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM metadata_jobs")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        let version: i64 = sqlx::query_scalar(
            "SELECT version FROM catalog_metadata \
            WHERE entity_type='recording' AND entity_id=1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(version, 1);
        service.metadata.close().await;
        pool.close().await;
    }
}
