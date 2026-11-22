//! Durable interval settings, version checks and bounded dispatch claims.

use crate::{
    automation::{AutomationError, Result, Schedule, ScheduleUpdate, Target},
    config::{AutomationConfig, automation::MAX_INTERVAL},
    library::enrichment::EntityKind,
};
use sqlx::{SqliteConnection, SqlitePool};

/// Scheduler-only target and mutable outcome, separate from public settings.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct Work {
    /// Durable schedule identity, never supplied by the dispatching client.
    pub(crate) id: i64,
    /// Library, discovery or metadata namespace.
    pub(crate) kind: String,
    /// Feed owner or administrator who enabled metadata work.
    pub(crate) owner_id: Option<i64>,
    /// Existing private feed, if this is a Discovery schedule.
    pub(crate) feed_id: Option<i64>,
    /// Catalog namespace for metadata work.
    pub(crate) entity_type: Option<String>,
    /// Existing catalog entity for metadata work.
    pub(crate) entity_id: Option<i64>,
    /// Public settings/status; interval is decorated before HTTP responses.
    #[sqlx(flatten)]
    pub(crate) schedule: Schedule,
}

/// Stable SQL identity without interpolating identifiers or user input.
fn key(target: Target) -> (&'static str, i64, &'static str, i64) {
    match target {
        Target::Library => ("library", 0, "", 0),
        Target::Discovery(id) => ("discovery", id, "", 0),
        Target::Metadata(kind, id) => ("metadata", 0, kind.as_str(), id),
    }
}

/// Default/minimum intervals are policy, not persisted per-user overrides.
pub(crate) fn intervals(
    config: &AutomationConfig,
    target: Target,
) -> (u32, u32) {
    match target {
        Target::Library => (config.library.scan_interval_seconds.max(60), 60),
        Target::Discovery(_) => (
            config.discovery.default_interval_seconds,
            config.discovery.min_interval_seconds,
        ),
        Target::Metadata(_, _) => (
            config.metadata.interval_seconds,
            config.metadata.interval_seconds,
        ),
    }
}

/// Check target existence and current ownership/role inside the same
/// transaction.
async fn access(
    c: &mut SqliteConnection,
    user: i64,
    target: Target,
) -> Result<()> {
    let allowed: bool = match target {
        Target::Discovery(id) => {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM discovery_feeds \
            WHERE id=? AND user_id=?)",
            )
            .bind(id)
            .bind(user)
            .fetch_one(c)
            .await?
        }
        Target::Metadata(kind, id) => sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=? AND role='admin') \
            AND EXISTS(SELECT 1 FROM catalog_metadata \
                WHERE entity_type=? AND entity_id=?)",
        )
        .bind(user)
        .bind(kind.as_str())
        .bind(id)
        .fetch_one(c)
        .await?,
        Target::Library => true,
    };
    if allowed {
        Ok(())
    } else {
        Err(AutomationError::NotFound)
    }
}

/// Read an optional configured row by its single namespace identity.
async fn find(
    c: &mut SqliteConnection,
    target: Target,
) -> Result<Option<Work>> {
    let (kind, feed, entity, id) = key(target);
    Ok(sqlx::query_as(
        "SELECT * FROM automation_schedules WHERE kind=? \
        AND COALESCE(feed_id,0)=? AND COALESCE(entity_type,'')=? \
        AND COALESCE(entity_id,0)=?",
    )
    .bind(kind)
    .bind(feed)
    .bind(entity)
    .bind(id)
    .fetch_optional(c)
    .await?)
}

/// Decorate current policy without mutating settings or hiding retained
/// intent.
async fn decorate(
    c: &mut SqliteConnection,
    config: &AutomationConfig,
    target: Target,
    work: Option<Work>,
) -> Result<Schedule> {
    let (default, minimum) = intervals(config, target);
    let owner = work.as_ref().and_then(|w| w.owner_id);
    let mut schedule = work
        .map(|w| w.schedule)
        .unwrap_or_else(|| Schedule::disabled(default));
    schedule.min_interval_seconds = minimum;
    schedule.max_interval_seconds = MAX_INTERVAL;
    schedule.effective_interval_seconds =
        schedule.interval_seconds.max(minimum);
    schedule.paused_reason =
        if !config.enabled {
            Some("Automatic dispatch is disabled in configuration".into())
        } else if matches!(target, Target::Metadata(_, _))
            && !config.metadata.enabled
        {
            Some("Automatic metadata proposals are disabled".into())
        } else if matches!(target, Target::Metadata(_, _)) && owner.is_some() {
            let admin: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=? AND role='admin')"
        ).bind(owner).fetch_one(c).await?;
            (!admin).then(|| {
                "Schedule creator is no longer an administrator".into()
            })
        } else {
            None
        };
    if schedule.paused_reason.is_none() && schedule.last_status == "paused" {
        schedule.paused_reason = schedule.message.clone();
    }
    Ok(schedule)
}

/// Read a virtual disabled schedule without inserting state for a GET request.
pub(crate) async fn get(
    pool: &SqlitePool,
    config: &AutomationConfig,
    user: i64,
    target: Target,
) -> Result<Schedule> {
    let mut tx = pool.begin().await?;
    access(&mut tx, user, target).await?;
    let work = find(&mut tx, target).await?;
    let schedule = decorate(&mut tx, config, target, work).await?;
    tx.commit().await?;
    Ok(schedule)
}

/// Atomically replace settings; running work remains attached through edits.
/// A different administrator explicitly saving metadata settings adopts them.
pub(crate) async fn set(
    pool: &SqlitePool,
    config: &AutomationConfig,
    user: i64,
    target: Target,
    input: ScheduleUpdate,
) -> Result<Schedule> {
    let (_, minimum) = intervals(config, target);
    if input.version < 0
        || !(minimum..=MAX_INTERVAL).contains(&input.interval_seconds)
        || matches!(target, Target::Library)
    {
        return Err(AutomationError::Invalid(
            "Invalid schedule version or interval",
        ));
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    access(&mut tx, user, target).await?;
    let current = find(&mut tx, target).await?;
    if current.as_ref().map_or(0, |w| w.schedule.version) != input.version {
        return Err(AutomationError::Conflict(
            "Schedule changed; reload settings",
        ));
    }
    let changed = current.as_ref().is_none_or(|w| {
        w.schedule.enabled != input.enabled
            || w.schedule.interval_seconds != input.interval_seconds
            || w.owner_id != Some(user)
    });
    if changed {
        let (kind, feed, entity, id) = key(target);
        let maximum = if kind == "discovery" {
            config.discovery.max_schedules_per_user
        } else {
            config.metadata.max_schedules
        };
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM automation_schedules WHERE kind=? \
            AND enabled=1 AND (?!='discovery' OR owner_id=?) \
            AND id!=?",
        )
        .bind(kind)
        .bind(kind)
        .bind(user)
        .bind(current.as_ref().map_or(0, |w| w.id))
        .fetch_one(&mut *tx)
        .await?;
        if input.enabled && count >= maximum {
            return Err(AutomationError::Conflict(
                "Enabled schedule limit reached",
            ));
        }
        if let Some(current) = &current {
            sqlx::query(
                "UPDATE automation_schedules SET version=version+1, \
                owner_id=?,enabled=?,interval_seconds=?, \
                message=CASE WHEN last_status IN ('dispatching','running') \
                    THEN message ELSE NULL END, \
                failures=CASE WHEN last_status IN ('dispatching','running') \
                    THEN failures ELSE 0 END, \
                last_status=CASE WHEN last_status IN \
                    ('dispatching','running') \
                    THEN last_status ELSE 'idle' END,next_run_at=CASE \
                WHEN ?=0 OR last_status IN ('dispatching','running') \
                    THEN NULL \
                ELSE unixepoch()+? END WHERE id=?",
            )
            .bind(user)
            .bind(input.enabled)
            .bind(input.interval_seconds)
            .bind(input.enabled)
            .bind(input.interval_seconds)
            .bind(current.id)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO automation_schedules \
                (kind,owner_id,feed_id,entity_type,entity_id,enabled, \
                    interval_seconds,next_run_at) VALUES (?,?,?,?,?,?,?, \
                    CASE WHEN ? THEN unixepoch()+? ELSE NULL END)",
            )
            .bind(kind)
            .bind(user)
            .bind((feed != 0).then_some(feed))
            .bind((!entity.is_empty()).then_some(entity))
            .bind((id != 0).then_some(id))
            .bind(input.enabled)
            .bind(input.interval_seconds)
            .bind(input.enabled)
            .bind(input.interval_seconds)
            .execute(&mut *tx)
            .await?;
        }
    }
    let work = find(&mut tx, target).await?;
    let schedule = decorate(&mut tx, config, target, work).await?;
    tx.commit().await?;
    Ok(schedule)
}

/// Synchronize only TOML-owned scan settings, preserving attached active work.
pub(crate) async fn initialize(
    pool: &SqlitePool,
    config: &AutomationConfig,
) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    // A crash between worker admission and attachment can leave a dispatch
    // without an ID. Worker recovery runs first; retry once after a short
    // delay.
    sqlx::query(
        "UPDATE automation_schedules SET last_status='failed', \
        failures=MIN(failures+1,32), \
        message='Dispatch interrupted by restart', \
        next_run_at=CASE WHEN enabled THEN unixepoch()+60 ELSE NULL END \
        WHERE last_status='dispatching'",
    )
    .execute(&mut *tx)
    .await?;
    let interval = config.library.scan_interval_seconds;
    sqlx::query(
        "INSERT INTO automation_schedules \
        (kind,enabled,interval_seconds,next_run_at) \
        VALUES ('library',?,?,CASE WHEN ? THEN unixepoch()+? ELSE NULL END) \
        ON CONFLICT(kind) WHERE kind='library' DO UPDATE SET \
        enabled=excluded.enabled,interval_seconds=excluded.interval_seconds, \
        version=automation_schedules.version+1,next_run_at=CASE \
            WHEN excluded.enabled=0 OR automation_schedules.last_status \
                IN ('dispatching','running') THEN NULL \
            ELSE unixepoch()+excluded.interval_seconds END \
        WHERE automation_schedules.enabled!=excluded.enabled \
            OR automation_schedules.interval_seconds \
                !=excluded.interval_seconds",
    )
    .bind(interval != 0)
    .bind(interval.max(60))
    .bind(interval != 0)
    .bind(interval.max(60))
    .execute(&mut *tx)
    .await?;
    // Raising operator bounds after restart must not let an old short interval
    // immediately launch another successful/failed target before the new
    // floor.
    for (kind, minimum) in [
        ("discovery", config.discovery.min_interval_seconds),
        ("metadata", config.metadata.interval_seconds),
    ] {
        sqlx::query(
            "UPDATE automation_schedules SET next_run_at= \
            MAX(next_run_at,last_run_at+?) WHERE kind=? AND enabled=1 \
            AND last_run_at IS NOT NULL AND interval_seconds<? \
            AND last_status IN ('completed','failed')",
        )
        .bind(minimum)
        .bind(kind)
        .bind(minimum)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Page selected metadata targets so administrators can find existing
/// settings.
pub(crate) async fn metadata_targets(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<(String, i64)>> {
    Ok(sqlx::query_as(
        "SELECT entity_type,entity_id FROM automation_schedules \
        WHERE kind='metadata' ORDER BY id LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?)
}

/// Reconstruct the validated target from a constrained storage row.
pub(crate) fn target(work: &Work) -> Result<Target> {
    match work.kind.as_str() {
        "library" => Ok(Target::Library),
        "discovery" => work
            .feed_id
            .map(Target::Discovery)
            .ok_or(AutomationError::NotFound),
        "metadata" => {
            let kind = match work.entity_type.as_deref() {
                Some("artist") => EntityKind::Artist,
                Some("release") => EntityKind::Release,
                Some("recording") => EntityKind::Recording,
                _ => return Err(AutomationError::NotFound),
            };
            work.entity_id
                .map(|id| Target::Metadata(kind, id))
                .ok_or(AutomationError::NotFound)
        }
        _ => Err(AutomationError::NotFound),
    }
}

/// Current native job outcome; an absent deleted job counts as interrupted.
pub(crate) async fn outcome(pool: &SqlitePool, work: &Work) -> Result<String> {
    let query = match work.kind.as_str() {
        "library" => "SELECT status FROM library_scans WHERE id=?",
        "discovery" => "SELECT status FROM discovery_jobs WHERE id=?",
        "metadata" => "SELECT status FROM metadata_jobs WHERE id=?",
        _ => return Err(AutomationError::NotFound),
    };
    Ok(sqlx::query_scalar(query)
        .bind(work.schedule.last_job_id)
        .fetch_optional(pool)
        .await?
        .unwrap_or_else(|| "interrupted".into()))
}

/// Poll a bounded set of attached native workers, independently of global
/// opt-in.
pub(crate) async fn running(pool: &SqlitePool) -> Result<Vec<Work>> {
    Ok(sqlx::query_as(
        "SELECT * FROM automation_schedules \
        WHERE last_status='running' ORDER BY last_run_at,id LIMIT 32",
    )
    .fetch_all(pool)
    .await?)
}

/// Select a fair bounded due batch; no backlog of missed intervals is
/// expanded.
pub(crate) async fn due(pool: &SqlitePool) -> Result<Vec<i64>> {
    // Admission is local and bounded. If a DB outage interrupted attachment,
    // re-arm its claim instead of leaving it stuck until a process restart.
    sqlx::query(
        "UPDATE automation_schedules SET last_status='failed', \
        failures=MIN(failures+1,32), \
        message='Dispatch attachment interrupted', \
        next_run_at=CASE WHEN enabled THEN unixepoch()+60 ELSE NULL END \
        WHERE last_status='dispatching' AND last_run_at<=unixepoch()-60",
    )
    .execute(pool)
    .await?;
    Ok(sqlx::query_scalar(
        "SELECT id FROM automation_schedules \
        WHERE enabled=1 AND next_run_at<=unixepoch() \
        AND last_status NOT IN ('running','dispatching') \
        ORDER BY next_run_at,id LIMIT 16",
    )
    .fetch_all(pool)
    .await?)
}

/// Claim one current due schedule, rechecking policy/role within the write
/// lock.
pub(crate) async fn claim(
    pool: &SqlitePool,
    config: &AutomationConfig,
    id: i64,
) -> Result<Option<Work>> {
    if !config.enabled {
        return Ok(None);
    }
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let work: Option<Work> = sqlx::query_as(
        "SELECT * FROM automation_schedules \
        WHERE id=? AND enabled=1 AND next_run_at<=unixepoch() \
        AND last_status NOT IN ('running','dispatching')",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(work) = work else {
        return Ok(None);
    };
    let allowed = if work.kind == "metadata" {
        config.metadata.enabled && sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=? AND role='admin')"
        ).bind(work.owner_id).fetch_one(&mut *tx).await?
    } else if work.kind == "discovery" {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 \
            FROM discovery_feeds WHERE id=? AND user_id=?)",
        )
        .bind(work.feed_id)
        .bind(work.owner_id)
        .fetch_one(&mut *tx)
        .await?
    } else {
        config.library.scan_interval_seconds > 0
    };
    if !allowed {
        sqlx::query(
            "UPDATE automation_schedules SET last_status='paused', \
            message='Automatic target is disabled or permission changed', \
            next_run_at=unixepoch()+300 WHERE id=?",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(None);
    }
    sqlx::query(
        "UPDATE automation_schedules SET last_status='dispatching', \
        next_run_at=NULL,last_run_at=unixepoch(),message=NULL WHERE id=?",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let work = sqlx::query_as("SELECT * FROM automation_schedules WHERE id=?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(work))
}

/// Bind the admitted native worker; settings edits never detach active work.
pub(crate) async fn attach(
    pool: &SqlitePool,
    id: i64,
    job: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE automation_schedules SET last_status='running', \
        last_job_id=? WHERE id=? AND last_status='dispatching'",
    )
    .bind(job)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Complete/defer/pause using current settings, preserving concurrent edits.
pub(crate) async fn settle(
    pool: &SqlitePool,
    config: &AutomationConfig,
    id: i64,
    status: &str,
    message: Option<&str>,
) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let work: Option<Work> =
        sqlx::query_as("SELECT * FROM automation_schedules WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(work) = work else {
        return Ok(());
    };
    let (_, minimum) = intervals(config, target(&work)?);
    let interval = work.schedule.interval_seconds.max(minimum);
    let failures = match status {
        "failed" => work.schedule.failures.saturating_add(1).min(32),
        "completed" => 0,
        _ => work.schedule.failures,
    };
    let delay = match status {
        "deferred" => 30,
        "paused" => 300,
        "failed" => interval
            .saturating_mul(1 << failures.min(5))
            .min(MAX_INTERVAL),
        _ => interval,
    };
    sqlx::query(
        "UPDATE automation_schedules SET last_status=?,failures=?, \
        message=?,next_run_at=CASE WHEN enabled THEN unixepoch()+? \
            ELSE NULL END WHERE id=?",
    )
    .bind(status)
    .bind(failures)
    .bind(message)
    .bind(delay)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    //! Version races, owner privacy, policy changes and durable timing.
    use super::*;
    use crate::discovery::{FeedRequest, GeneratedFeed};

    /// Small valid policy makes timing assertions independent of wall time.
    pub(crate) fn config() -> AutomationConfig {
        let mut config = AutomationConfig {
            enabled: true,
            ..AutomationConfig::default()
        };
        config.discovery.min_interval_seconds = 60;
        config.discovery.default_interval_seconds = 120;
        config
    }

    /// Complete a feed fixture without contacting providers or marking
    /// priority.
    pub(crate) async fn feed(
        pool: &SqlitePool,
        user: i64,
        request: FeedRequest,
    ) -> i64 {
        let job = crate::db::discovery::admit(pool, user, &request, 0)
            .await
            .unwrap();
        crate::db::discovery::complete(
            pool,
            &job,
            GeneratedFeed {
                title: "Fixture".into(),
                source_url: None,
                items: vec![],
                warnings: vec![],
            },
        )
        .await
        .unwrap();
        sqlx::query("UPDATE discovery_jobs SET created_at=0 WHERE id=?")
            .bind(job.id)
            .execute(pool)
            .await
            .unwrap();
        job.feed_id
    }

    /// Enable/disable a target with complete settings and explicit version.
    pub(crate) fn update(
        version: i64,
        enabled: bool,
        interval: u32,
    ) -> ScheduleUpdate {
        ScheduleUpdate {
            version,
            enabled,
            interval_seconds: interval,
        }
    }

    /// Make configured work overdue without sleeping or replaying fake timers.
    pub(crate) async fn overdue(pool: &SqlitePool) {
        sqlx::query(
            "UPDATE automation_schedules SET next_run_at=unixepoch()-9999 \
            WHERE enabled=1 AND last_status NOT IN ('running','dispatching')",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// Privacy includes administrators; optimistic settings avoid lost
    /// updates.
    #[tokio::test]
    async fn preserves_owner_privacy_versions_noops_and_capacity() {
        let (_dir, pool) = crate::db::discovery::tests::setup().await;
        let id = feed(&pool, 1, FeedRequest::Personal {}).await;
        let second = feed(&pool, 1, FeedRequest::Rediscover {}).await;
        let mut config = config();
        config.discovery.max_schedules_per_user = 1;
        let target = Target::Discovery(id);
        assert_eq!(get(&pool, &config, 1, target).await.unwrap().version, 0);
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM automation_schedules")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
        sqlx::query("UPDATE users SET role='admin' WHERE id=2")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            get(&pool, &config, 2, target).await,
            Err(AutomationError::NotFound)
        ));
        assert!(matches!(
            set(&pool, &config, 2, target, update(0, true, 60)).await,
            Err(AutomationError::NotFound)
        ));
        let first = set(&pool, &config, 1, target, update(0, true, 60))
            .await
            .unwrap();
        assert_eq!(first.version, 1);
        assert_eq!(
            set(&pool, &config, 1, target, update(1, true, 60))
                .await
                .unwrap()
                .version,
            1
        );
        assert!(
            set(
                &pool,
                &config,
                1,
                Target::Discovery(second),
                update(0, true, 60)
            )
            .await
            .is_err()
        );
        let (a, b) = tokio::join!(
            set(&pool, &config, 1, target, update(1, true, 120)),
            set(&pool, &config, 1, target, update(1, true, 180))
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(get(&pool, &config, 1, target).await.unwrap().version, 2);
        set(&pool, &config, 1, target, update(2, false, 120))
            .await
            .unwrap();
        set(
            &pool,
            &config,
            1,
            Target::Discovery(second),
            update(0, true, 60),
        )
        .await
        .unwrap();
        assert!(
            set(&pool, &config, 1, target, update(3, true, 59))
                .await
                .is_err()
        );
        pool.close().await;
    }

    /// Claims are exclusive, settings edits retain active IDs and backoff is
    /// bounded.
    #[tokio::test]
    async fn claims_once_and_reschedules_using_current_settings() {
        let (_dir, pool) = crate::db::discovery::tests::setup().await;
        let id = feed(&pool, 1, FeedRequest::Personal {}).await;
        let config = config();
        let target = Target::Discovery(id);
        set(&pool, &config, 1, target, update(0, true, 60))
            .await
            .unwrap();
        overdue(&pool).await;
        let id = due(&pool).await.unwrap()[0];
        let (a, b) =
            tokio::join!(claim(&pool, &config, id), claim(&pool, &config, id));
        assert_ne!(a.unwrap().is_some(), b.unwrap().is_some());
        attach(&pool, id, 123).await.unwrap();
        let disabled = set(&pool, &config, 1, target, update(1, false, 120))
            .await
            .unwrap();
        assert_eq!(disabled.last_status, "running");
        assert_eq!(disabled.last_job_id, Some(123));
        settle(&pool, &config, id, "completed", None).await.unwrap();
        assert_eq!(
            get(&pool, &config, 1, target).await.unwrap().next_run_at,
            None
        );
        set(&pool, &config, 1, target, update(2, true, 120))
            .await
            .unwrap();
        settle(&pool, &config, id, "failed", Some("Safe failure"))
            .await
            .unwrap();
        let failed = get(&pool, &config, 1, target).await.unwrap();
        assert_eq!(failed.failures, 1);
        let wait: i64 = sqlx::query_scalar(
            "SELECT next_run_at-unixepoch() FROM automation_schedules \
            WHERE id=?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!((239..=240).contains(&wait));
        settle(&pool, &config, id, "deferred", Some("Busy"))
            .await
            .unwrap();
        assert_eq!(get(&pool, &config, 1, target).await.unwrap().failures, 1);
        settle(&pool, &config, id, "completed", None).await.unwrap();
        assert_eq!(get(&pool, &config, 1, target).await.unwrap().failures, 0);
        sqlx::query(
            "UPDATE automation_schedules SET last_run_at=unixepoch(), \
            next_run_at=unixepoch()+1 WHERE id=?",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        let mut raised = config.clone();
        raised.discovery.min_interval_seconds = 300;
        raised.discovery.default_interval_seconds = 600;
        initialize(&pool, &raised).await.unwrap();
        let read = get(&pool, &raised, 1, target).await.unwrap();
        assert_eq!(read.interval_seconds, 120);
        assert_eq!(read.effective_interval_seconds, 300);
        let wait: i64 = sqlx::query_scalar(
            "SELECT next_run_at-unixepoch() FROM automation_schedules \
            WHERE id=?",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!((299..=300).contains(&wait));
        pool.close().await;
    }

    /// Explicit metadata ownership can be adopted after its creator is
    /// demoted.
    #[tokio::test]
    async fn rechecks_admin_role_and_retains_schedules_across_restart() {
        let (dir, pool) = crate::db::discovery::tests::setup().await;
        sqlx::query("UPDATE users SET role='admin'")
            .execute(&pool)
            .await
            .unwrap();
        let mut config = config();
        config.metadata.enabled = true;
        config.metadata.max_schedules = 1;
        let target = Target::Metadata(EntityKind::Recording, 1);
        set(&pool, &config, 1, target, update(0, true, 86400))
            .await
            .unwrap();
        assert!(
            set(
                &pool,
                &config,
                2,
                Target::Metadata(EntityKind::Artist, 1),
                update(0, true, 86400)
            )
            .await
            .is_err()
        );
        sqlx::query("UPDATE users SET role='member' WHERE id=1")
            .execute(&pool)
            .await
            .unwrap();
        overdue(&pool).await;
        let id = due(&pool).await.unwrap()[0];
        assert!(claim(&pool, &config, id).await.unwrap().is_none());
        assert!(
            get(&pool, &config, 2, target)
                .await
                .unwrap()
                .paused_reason
                .is_some()
        );
        let adopted = set(&pool, &config, 2, target, update(1, true, 86400))
            .await
            .unwrap();
        assert_eq!(adopted.version, 2);
        pool.close().await;
        let pool = crate::db::connect(&dir.path().join("discovery.db"))
            .await
            .unwrap();
        initialize(&pool, &config).await.unwrap();
        let after = get(&pool, &config, 2, target).await.unwrap();
        assert!(after.enabled);
        assert_eq!(after.version, 2);
        assert!(after.paused_reason.is_none());
        sqlx::query("DELETE FROM recordings WHERE id=1")
            .execute(&pool)
            .await
            .unwrap();
        assert!(metadata_targets(&pool, 50, 0).await.unwrap().is_empty());
        pool.close().await;
    }
}
