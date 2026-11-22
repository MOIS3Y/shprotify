//! Durable acquisition admission, ownership, cancellation and source
//! selection.

use crate::{
    config::DownloadsConfig,
    discovery::CandidateKind,
    downloads::{
        DownloadError, DownloadJob, Result, SearchRequest, Source, Target,
    },
};
use sqlx::{SqlitePool, types::Json};

/// Internal work item; sensitive intent is never returned with public job
/// state.
#[derive(Debug, sqlx::FromRow)]
pub(crate) struct Work {
    /// Public lifecycle information.
    #[sqlx(flatten)]
    pub(crate) job: DownloadJob,
    /// Authenticated owner captured at admission.
    pub(crate) user_id: i64,
    /// Validated desired catalog metadata.
    #[sqlx(json)]
    pub(crate) target: Target,
}

/// Capture intent without trusting a caller-supplied catalog identity
/// snapshot.
pub(crate) async fn target(
    pool: &SqlitePool,
    user: i64,
    request: SearchRequest,
) -> Result<Target> {
    let mut value = Target {
        request: request.clone(),
        title: String::new(),
        artists: vec![],
        recording_id: None,
        candidate_id: None,
        candidate: None,
        source_duration_seconds: None,
    };
    match request {
        SearchRequest::Recording { id } => {
            let recording = super::recordings::find(pool, id)
                .await?
                .ok_or(DownloadError::NotFound)?;
            value.title = recording.title;
            value.recording_id = Some(id);
            value.artists = sqlx::query_scalar(
                "SELECT a.name FROM recording_artists r JOIN artists a \
                ON a.id=r.artist_id WHERE r.recording_id=? ORDER BY r.position"
            ).bind(id).fetch_all(pool).await?;
        }
        SearchRequest::Candidate { id } => {
            let item = super::discovery::candidate(pool, user, id)
                .await
                .map_err(|e| match e {
                    super::discovery::DiscoveryError::NotFound => {
                        DownloadError::NotFound
                    }
                    super::discovery::DiscoveryError::Storage(e) => e.into(),
                    super::discovery::DiscoveryError::Json(e) => e.into(),
                    _ => DownloadError::Invalid("Invalid discovery candidate"),
                })?;
            if item.candidate.kind != CandidateKind::Recording {
                return Err(DownloadError::Invalid(
                    "Choose an individual recording, not a collection",
                ));
            }
            value.title = item.candidate.title.clone();
            value.artists = item
                .candidate
                .artists
                .iter()
                .map(|a| a.name.clone())
                .collect();
            value.candidate_id = Some(id);
            value.recording_id = item.local_id;
            value.candidate = Some(item.candidate);
        }
        SearchRequest::Query { title, artists } => {
            if artists.len() > 20 {
                return Err(DownloadError::Invalid(
                    "At most 20 query credits",
                ));
            }
            value.title = title.trim().into();
            value.artists = artists.iter().map(|s| s.trim().into()).collect();
        }
    }
    value.validate()?;
    Ok(value)
}

/// Fetch only owned jobs; administrators cannot read another user's activity.
pub(crate) async fn job(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<DownloadJob> {
    sqlx::query_as("SELECT * FROM download_jobs WHERE user_id=? AND id=?")
        .bind(user)
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(DownloadError::NotFound)
}

/// Page an account's job history newest first, including terminal failures.
pub(crate) async fn jobs(
    pool: &SqlitePool,
    user: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<DownloadJob>> {
    Ok(sqlx::query_as(
        "SELECT * FROM download_jobs WHERE user_id=? \
        ORDER BY id DESC LIMIT ? OFFSET ?",
    )
    .bind(user)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?)
}

/// Load worker-only intent; the caller already owns this service lifecycle.
pub(crate) async fn work(pool: &SqlitePool, id: i64) -> Result<Work> {
    Ok(sqlx::query_as("SELECT * FROM download_jobs WHERE id=?")
        .bind(id)
        .fetch_one(pool)
        .await?)
}

/// Reserve bounded queue capacity and both source/target identities
/// atomically.
pub(crate) async fn enqueue(
    pool: &SqlitePool,
    config: &DownloadsConfig,
    user: i64,
    target: &Target,
    video: Option<&str>,
    retry_of: Option<i64>,
) -> Result<DownloadJob> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let (total, personal, recent): (i64, i64, bool) = sqlx::query_as(
        "SELECT COALESCE(sum(status IN \
            ('queued','running','verifying','importing')),0), \
        COALESCE(sum(user_id=? AND status IN \
            ('queued','running','verifying','importing')),0), \
        EXISTS(SELECT 1 FROM download_jobs WHERE user_id=? \
            AND operation='search' AND created_at > unixepoch()-?) \
        FROM download_jobs",
    )
    .bind(user)
    .bind(user)
    .bind(config.request_interval_seconds)
    .fetch_one(&mut *tx)
    .await?;
    if total >= config.max_pending_jobs || personal >= config.max_user_jobs {
        return Err(DownloadError::Conflict(
            "Download queue capacity reached",
        ));
    }
    if recent && video.is_none() {
        return Err(DownloadError::Conflict(
            "Wait before submitting another job",
        ));
    }
    if let Some(video) = video {
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM download_imports i \
            JOIN download_jobs j ON j.id=i.job_id \
            WHERE j.video_id=? OR j.target_key=?)",
        )
        .bind(video)
        .bind(target.key())
        .fetch_one(&mut *tx)
        .await?;
        if pending {
            return Err(DownloadError::Conflict(
                "Import awaits server recovery",
            ));
        }
    }
    let row = sqlx::query_as(
        "INSERT INTO download_jobs \
        (user_id, operation, target, target_key, video_id, retry_of) \
        VALUES (?, ?, ?, ?, ?, ?) RETURNING *",
    )
    .bind(user)
    .bind(if video.is_some() {
        "download"
    } else {
        "search"
    })
    .bind(Json(target))
    .bind(video.map(|_| target.key()))
    .bind(video)
    .bind(retry_of)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if e.as_database_error()
            .is_some_and(|e| e.is_unique_violation())
        {
            DownloadError::Conflict(
                "This source or recording is already queued",
            )
        } else {
            e.into()
        }
    })?;
    tx.commit().await?;
    Ok(row)
}

/// A member may choose only results from their own recent completed search.
pub(crate) async fn select(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<(Target, String, Option<i64>)> {
    let row: Option<(Json<Target>, String, Option<i64>)> = sqlx::query_as(
        "SELECT j.target,s.video_id,s.duration_seconds \
        FROM download_sources s \
        JOIN download_jobs j ON j.id=s.job_id WHERE s.id=? AND j.user_id=? \
        AND j.status='completed' AND j.created_at>=unixepoch()-3600",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(pool)
    .await?;
    row.map(|(t, v, d)| (t.0, v, d))
        .ok_or(DownloadError::NotFound)
}

/// Read bounded source results without exposing another account's search.
pub(crate) async fn sources(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<Vec<Source>> {
    let job = job(pool, user, id).await?;
    if job.operation != "search" {
        return Err(DownloadError::Invalid("This is not a source search"));
    }
    Ok(sqlx::query_as(
        "SELECT *, \
        'https://www.youtube.com/watch?v='||video_id url \
        FROM download_sources WHERE job_id=? ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?)
}

/// Complete a search and publish all results atomically, including an empty
/// set.
pub(crate) async fn searched(
    pool: &SqlitePool,
    id: i64,
    sources: &[Source],
) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    for source in sources {
        sqlx::query(
            "INSERT INTO download_sources \
            (job_id,video_id,title,channel,duration_seconds) \
            VALUES (?,?,?,?,?)",
        )
        .bind(id)
        .bind(&source.video_id)
        .bind(&source.title)
        .bind(&source.channel)
        .bind(source.duration_seconds)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE download_jobs SET \
        status=CASE WHEN cancel_requested THEN 'cancelled' \
            ELSE 'completed' END, \
        finished_at=unixepoch() WHERE id=? AND status='running'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Cooperatively cancel queued/running work; publication is an atomic
/// boundary.
pub(crate) async fn cancel(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<DownloadJob> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let current: DownloadJob =
        sqlx::query_as("SELECT * FROM download_jobs WHERE user_id=? AND id=?")
            .bind(user)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(DownloadError::NotFound)?;
    if current.status == "importing" {
        return Err(DownloadError::Conflict(
            "Import has started; wait for completion",
        ));
    }
    if matches!(current.status.as_str(), "queued" | "running" | "verifying") {
        sqlx::query(
            "UPDATE download_jobs SET cancel_requested=1, \
            status=CASE WHEN status='queued' THEN 'cancelled' \
                ELSE status END, \
            finished_at=CASE WHEN status='queued' THEN unixepoch() \
                ELSE finished_at END WHERE id=?",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    job(pool, user, id).await
}

/// Claim queued work only once and reject a cancellation accepted beforehand.
pub(crate) async fn claim(pool: &SqlitePool, id: i64) -> Result<bool> {
    Ok(sqlx::query(
        "UPDATE download_jobs SET status='running' \
        WHERE id=? AND status='queued' AND cancel_requested=0",
    )
    .bind(id)
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

/// Persist approximate disk progress; never report bytes as a completion
/// signal.
pub(crate) async fn progress(
    pool: &SqlitePool,
    id: i64,
    bytes: u64,
) -> Result<bool> {
    let cancel: bool = sqlx::query_scalar(
        "UPDATE download_jobs SET downloaded_bytes=? WHERE id=? \
        RETURNING cancel_requested",
    )
    .bind(i64::try_from(bytes).unwrap_or(i64::MAX))
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(cancel)
}

/// Persist a safe terminal state after process cleanup or interrupted work.
pub(crate) async fn finish(
    pool: &SqlitePool,
    id: i64,
    status: &str,
    message: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE download_jobs SET status=CASE \
        WHEN cancel_requested=1 AND ?='failed' THEN 'cancelled' ELSE ? END, \
        message=?,finished_at=unixepoch() WHERE id=? \
        AND status NOT IN ('completed','cancelled')",
    )
    .bind(status)
    .bind(status)
    .bind(message)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark unjournaled active jobs interrupted; queued intent survives restarts.
pub(crate) async fn recover(pool: &SqlitePool) -> Result<Vec<i64>> {
    sqlx::query(
        "UPDATE download_jobs SET status='interrupted', \
        message='Server stopped before completion; retry explicitly', \
        finished_at=unixepoch() WHERE status IN \
            ('running','verifying','importing') \
        AND id NOT IN (SELECT job_id FROM download_imports)",
    )
    .execute(pool)
    .await?;
    Ok(sqlx::query_scalar(
        "SELECT id FROM download_jobs \
        WHERE status='queued' ORDER BY id",
    )
    .fetch_all(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    //! Admission races, privacy, source expiry and cancellation boundaries.
    use super::*;

    /// Bounded queue admission and global source uniqueness survive races.
    #[tokio::test]
    async fn serializes_duplicates_and_limits_without_leaking_owned_searches()
    {
        let f = crate::jobs::downloads::tests::Fixture::new().await;
        f.service.close().await;
        let target = target(
            &f.pool,
            1,
            crate::jobs::downloads::tests::Fixture::query(),
        )
        .await
        .unwrap();
        let (a, b) = tokio::join!(
            enqueue(&f.pool, &f.config, 1, &target, Some("abcdefghijk"), None),
            enqueue(&f.pool, &f.config, 2, &target, Some("abcdefghijk"), None)
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let (owner, job) = if let Ok(job) = a {
            (1, job)
        } else {
            (2, b.unwrap())
        };
        let other = if owner == 1 { 2 } else { 1 };
        assert!(matches!(
            super::job(&f.pool, other, job.id).await,
            Err(DownloadError::NotFound)
        ));
        assert!(matches!(
            cancel(&f.pool, other, job.id).await,
            Err(DownloadError::NotFound)
        ));
        let mut config = f.config.clone();
        config.max_pending_jobs = 1;
        assert!(
            enqueue(&f.pool, &config, other, &target, None, None)
                .await
                .is_err()
        );
        assert_eq!(
            cancel(&f.pool, owner, job.id).await.unwrap().status,
            "cancelled"
        );
        assert!(!claim(&f.pool, job.id).await.unwrap());
        let search = enqueue(&f.pool, &config, other, &target, None, None)
            .await
            .unwrap();
        claim(&f.pool, search.id).await.unwrap();
        let result = Source {
            id: 0,
            job_id: 0,
            video_id: "abcdefghijk".into(),
            title: "Source".into(),
            url: "https://www.youtube.com/watch?v=abcdefghijk".into(),
            channel: None,
            duration_seconds: Some(1),
        };
        searched(&f.pool, search.id, &[result]).await.unwrap();
        let source = sources(&f.pool, other, search.id).await.unwrap()[0].id;
        assert!(select(&f.pool, other, source).await.is_ok());
        assert!(matches!(
            select(&f.pool, owner, source).await,
            Err(DownloadError::NotFound)
        ));
        sqlx::query(
            "UPDATE download_jobs SET created_at=unixepoch()-3601 WHERE id=?",
        )
        .bind(search.id)
        .execute(&f.pool)
        .await
        .unwrap();
        assert!(select(&f.pool, other, source).await.is_err());
        f.pool.close().await;
    }

    /// A source selection cannot silently follow an updated candidate MBID.
    #[tokio::test]
    async fn rejects_changed_identity_after_source_search() {
        let f = crate::jobs::downloads::tests::Fixture::new().await;
        let mut item = crate::db::discovery::tests::suggestion(
            "lastfm",
            "identity-change",
            "Candidate",
            None,
        )
        .candidate;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO discovery_candidates (identity_key,kind,document) \
            VALUES ('change','recording',?) RETURNING id",
        )
        .bind(serde_json::to_string(&item).unwrap())
        .fetch_one(&f.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO discovery_feedback \
            (user_id,candidate_id,version,hidden,wanted,changed_at) \
            VALUES (1,?,1,0,1,strftime('%Y-%m-%dT%H:%M:%fZ','now'))",
        )
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
        let search = f
            .service
            .search(1, SearchRequest::Candidate { id })
            .await
            .unwrap();
        assert_eq!(f.wait(1, search.id).await.status, "completed");
        let source = sources(&f.pool, 1, search.id).await.unwrap()[0].id;
        item.mbid = Some(crate::db::discovery::tests::MBID.into());
        sqlx::query(
            "UPDATE discovery_candidates SET mbid=?,document=? WHERE id=?",
        )
        .bind(&item.mbid)
        .bind(serde_json::to_string(&item).unwrap())
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
        assert!(matches!(
            f.service.download(1, source).await,
            Err(DownloadError::Conflict(_))
        ));
        f.service.close().await;
        f.pool.close().await;
    }

    /// Verifying work is cancellable, while a journaled import is indivisible.
    #[tokio::test]
    async fn cancellation_races_the_import_boundary_atomically() {
        let f = crate::jobs::downloads::tests::Fixture::new().await;
        f.service.close().await;
        let target = target(
            &f.pool,
            1,
            crate::jobs::downloads::tests::Fixture::query(),
        )
        .await
        .unwrap();
        let job =
            enqueue(&f.pool, &f.config, 1, &target, Some("abcdefghijk"), None)
                .await
                .unwrap();
        sqlx::query("UPDATE download_jobs SET status='verifying' WHERE id=?")
            .bind(job.id)
            .execute(&f.pool)
            .await
            .unwrap();
        let hash = "a".repeat(64);
        let (cancelled, journaled) = tokio::join!(
            cancel(&f.pool, 1, job.id),
            crate::db::download_import::journal(
                &f.pool,
                job.id,
                &hash,
                "/unused/path.wav"
            )
        );
        assert_ne!(cancelled.is_ok(), journaled.is_ok());
        let work = work(&f.pool, job.id).await.unwrap();
        assert!(work.job.cancel_requested || work.job.status == "importing");
        f.pool.close().await;
    }
}
