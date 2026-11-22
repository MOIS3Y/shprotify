//! Transactional managed-file registration and discovery reconciliation.
//!
//! File publication happens between journal creation and this transaction.
//! Replaying a journal is idempotent; unrelated catalog metadata is preserved.

use super::downloads::Work;
use crate::{
    downloads::{
        DownloadError, Result, Target,
        policy::{DuplicatePolicy, ImportPolicy},
    },
    library::metadata::ScannedFile,
};
use sqlx::{SqliteConnection, SqlitePool};

/// Store publication intent only if cancellation has not won the boundary.
pub(crate) async fn journal(
    pool: &SqlitePool,
    id: i64,
    hash: &str,
    path: &str,
) -> Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM file_removals \
            WHERE sha256=? AND status='pending' AND remove_file=1)",
    )
    .bind(hash)
    .fetch_one(&mut *tx)
    .await?;
    if pending {
        return Err(DownloadError::Conflict("File removal is pending"));
    }
    let changed = sqlx::query(
        "UPDATE download_jobs SET status='importing' \
        WHERE id=? AND status='verifying' AND cancel_requested=0",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(DownloadError::Conflict("Download was cancelled"));
    }
    sqlx::query(
        "INSERT INTO download_imports (job_id,sha256,path) VALUES (?,?,?)",
    )
    .bind(id)
    .bind(hash)
    .bind(path)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Look for durable exact source/content identity before creating any rows.
pub(crate) async fn existing(
    pool: &SqlitePool,
    video: &str,
    hash: Option<&str>,
) -> Result<Option<(i64, i64)>> {
    Ok(sqlx::query_as(
        "SELECT recording_id,file_id FROM downloaded_sources \
        WHERE video_id=? UNION ALL \
        SELECT f.recording_id,f.id FROM audio_files f \
        JOIN audio_file_hashes h ON h.file_id=f.id WHERE h.sha256=? LIMIT 1",
    )
    .bind(video)
    .bind(hash)
    .fetch_optional(pool)
    .await?)
}

/// Reject disallowed catalog mutations before contacting a
/// provider/publishing.
/// The import transaction repeats these checks to close concurrent-write
/// races.
pub(crate) async fn check_policy(
    pool: &SqlitePool,
    target: &Target,
    policy: &ImportPolicy,
) -> Result<()> {
    let recording = if let Some(id) = target.recording_id {
        Some(id)
    } else if let Some(candidate) = target.candidate_id {
        sqlx::query_scalar(
            "SELECT recording_id FROM discovery_download_links \
            WHERE candidate_id=?",
        )
        .bind(candidate)
        .fetch_optional(pool)
        .await?
    } else {
        None
    };
    if recording.is_none() && !policy.create_recordings {
        return Err(DownloadError::Conflict(
            "Import policy requires an explicitly selected existing recording",
        ));
    }
    if !policy.allow_alternate_files && recording.is_some() {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM audio_files \
                    WHERE recording_id=?)",
        )
        .bind(recording)
        .fetch_one(pool)
        .await?;
        if exists {
            return Err(DownloadError::Conflict(
                "Import policy rejects alternative files for this track",
            ));
        }
    }
    Ok(())
}

/// Reject a source already associated with a different explicit catalog
/// target.
fn compatible(target: &Target, recording: i64) -> Result<()> {
    if target.recording_id.is_some_and(|id| id != recording) {
        return Err(DownloadError::Conflict(
            "Source belongs to another recording; ask an administrator",
        ));
    }
    Ok(())
}

/// Reuse a confirmed local artifact without creating or downloading a copy.
/// The caller checks its on-disk hash before invoking this function.
pub(crate) async fn reuse(
    pool: &SqlitePool,
    work: &Work,
    recording: i64,
    file: i64,
    policy: &ImportPolicy,
) -> Result<()> {
    if policy.duplicates == DuplicatePolicy::Reject {
        return Err(DownloadError::Conflict(
            "Import policy rejects duplicates",
        ));
    }
    compatible(&work.target, recording)?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let state: (String, bool) = sqlx::query_as(
        "SELECT status,cancel_requested FROM download_jobs WHERE id=?",
    )
    .bind(work.job.id)
    .fetch_one(&mut *tx)
    .await?;
    if state.1 || !matches!(state.0.as_str(), "running" | "verifying") {
        return Err(DownloadError::Conflict("Download was cancelled"));
    }
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM file_removals \
            WHERE file_id=? AND status='pending')",
    )
    .bind(file)
    .fetch_one(&mut *tx)
    .await?;
    if pending {
        return Err(DownloadError::Conflict("File removal is pending"));
    }
    reconcile(&mut tx, &work.target, recording).await?;
    provenance(&mut tx, work, recording, file).await?;
    tx.commit().await?;
    Ok(())
}

/// Finalize a journaled, validated object and all its catalog references.
pub(crate) async fn commit(
    pool: &SqlitePool,
    work: &Work,
    source: ScannedFile,
    policy: &ImportPolicy,
) -> Result<()> {
    let hash = source.sha256.as_deref().ok_or(DownloadError::Failed(
        "Audio verification did not produce a content hash",
    ))?;
    let video =
        work.job.video_id.as_deref().ok_or(DownloadError::Invalid(
            "Search jobs cannot import audio",
        ))?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let intent: (String, String) = sqlx::query_as(
        "SELECT sha256,path FROM download_imports WHERE job_id=?",
    )
    .bind(work.job.id)
    .fetch_one(&mut *tx)
    .await?;
    if intent.0 != hash || intent.1 != source.path {
        return Err(DownloadError::Failed(
            "Published audio failed verification",
        ));
    }
    let duplicate: Option<(i64, i64)> = sqlx::query_as(
        "SELECT recording_id,file_id FROM downloaded_sources WHERE video_id=? \
        UNION ALL SELECT f.recording_id,f.id FROM audio_files f \
        JOIN audio_file_hashes h ON h.file_id=f.id WHERE h.sha256=? LIMIT 1",
    )
    .bind(video)
    .bind(hash)
    .fetch_optional(&mut *tx)
    .await?;
    let (recording, file) = if let Some(pair) = duplicate {
        if policy.duplicates == DuplicatePolicy::Reject {
            return Err(DownloadError::Conflict(
                "Import policy rejects duplicates",
            ));
        }
        compatible(&work.target, pair.0)?;
        pair
    } else {
        let recording = resolve(&mut tx, &work.target, policy).await?;
        if !policy.allow_alternate_files {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM audio_files \
                    WHERE recording_id=?)",
            )
            .bind(recording)
            .fetch_one(&mut *tx)
            .await?;
            if exists {
                return Err(DownloadError::Conflict(
                    "Import policy rejects alternative files for this track",
                ));
            }
        }
        let p = &source.properties;
        let file: i64 = sqlx::query_scalar(
            "INSERT INTO audio_files \
            (recording_id,path,format,size_bytes,duration_ms,sample_rate_hz, \
            channels,bit_depth,bitrate_bps) VALUES (?,?,?,?,?,?,?,?,?) \
            RETURNING id",
        )
        .bind(recording)
        .bind(&source.path)
        .bind(&source.format)
        .bind(p.size_bytes)
        .bind(p.duration_ms)
        .bind(p.sample_rate_hz)
        .bind(p.channels)
        .bind(p.bit_depth)
        .bind(p.bitrate_bps)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO audio_file_tags (file_id,tags) VALUES (?,?)")
            .bind(file)
            .bind(serde_json::to_string(&source.tags)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO audio_file_hashes (file_id,sha256) VALUES (?,?)",
        )
        .bind(file)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
        if let Some(state) = &source.state {
            sqlx::query(
                "INSERT INTO audio_file_states (file_id,state) VALUES (?,?)",
            )
            .bind(file)
            .bind(serde_json::to_string(state)?)
            .execute(&mut *tx)
            .await?;
        }
        (recording, file)
    };
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM file_removals \
            WHERE file_id=? AND status='pending')",
    )
    .bind(file)
    .fetch_one(&mut *tx)
    .await?;
    if pending {
        return Err(DownloadError::Conflict("File removal is pending"));
    }
    reconcile(&mut tx, &work.target, recording).await?;
    provenance(&mut tx, work, recording, file).await?;
    sqlx::query("DELETE FROM download_imports WHERE job_id=?")
        .bind(work.job.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Create only a requested recording; never claim a candidate's external MBID.
async fn resolve(
    c: &mut SqliteConnection,
    target: &Target,
    policy: &ImportPolicy,
) -> Result<i64> {
    if let Some(id) = target.recording_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM recordings WHERE id=?)",
        )
        .bind(id)
        .fetch_one(c)
        .await?;
        return if exists {
            Ok(id)
        } else {
            Err(DownloadError::NotFound)
        };
    }
    if let Some(candidate) = target.candidate_id {
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT recording_id FROM discovery_download_links \
            WHERE candidate_id=?",
        )
        .bind(candidate)
        .fetch_optional(&mut *c)
        .await?;
        if let Some(id) = existing {
            return Ok(id);
        }
    }
    if !policy.create_recordings {
        return Err(DownloadError::Conflict(
            "Import policy requires an explicitly selected existing recording",
        ));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO recordings (title) VALUES (?) RETURNING id",
    )
    .bind(&target.title)
    .fetch_one(&mut *c)
    .await?;
    let mut seen = std::collections::HashSet::new();
    let mut position = 0_i64;
    for name in &target.artists {
        let matches: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM artists WHERE name=? LIMIT 2")
                .bind(name)
                .fetch_all(&mut *c)
                .await?;
        let artist = if matches.len() == 1 {
            matches[0]
        } else {
            sqlx::query_scalar(
                "INSERT INTO artists (name) VALUES (?) RETURNING id",
            )
            .bind(name)
            .fetch_one(&mut *c)
            .await?
        };
        if seen.insert(artist) {
            sqlx::query(
                "INSERT INTO recording_artists \
                (recording_id,artist_id,position) VALUES (?,?,?)",
            )
            .bind(id)
            .bind(artist)
            .bind(position)
            .execute(&mut *c)
            .await?;
            position += 1;
        }
    }
    Ok(id)
}

/// Link a selected candidate and migrate retained reactions conservatively.
/// A permanent native decision tombstone protects later explicit clears too.
async fn reconcile(
    c: &mut SqliteConnection,
    target: &Target,
    recording: i64,
) -> Result<()> {
    let Some(candidate) = target.candidate_id else {
        return Ok(());
    };
    let current: String = sqlx::query_scalar(
        "SELECT document FROM discovery_candidates WHERE id=?",
    )
    .bind(candidate)
    .fetch_optional(&mut *c)
    .await?
    .ok_or(DownloadError::NotFound)?;
    let current: crate::discovery::CandidateData =
        serde_json::from_str(&current)?;
    if target.candidate.as_ref().is_some_and(|old| {
        old.kind != current.kind
            || old.mbid != current.mbid
            || old.title != current.title
            || old
                .artists
                .iter()
                .map(|a| &a.name)
                .ne(current.artists.iter().map(|a| &a.name))
    }) {
        return Err(DownloadError::Conflict(
            "Discovery identity changed; search sources again",
        ));
    }
    let linked: Option<i64> = sqlx::query_scalar(
        "SELECT recording_id FROM discovery_download_links \
            WHERE candidate_id=?",
    )
    .bind(candidate)
    .fetch_optional(&mut *c)
    .await?;
    if linked.is_some_and(|id| id != recording) {
        return Err(DownloadError::Conflict(
            "Candidate already has another recording",
        ));
    }
    sqlx::query(
        "INSERT INTO discovery_download_links VALUES (?,?) \
        ON CONFLICT(candidate_id) DO NOTHING",
    )
    .bind(candidate)
    .bind(recording)
    .execute(&mut *c)
    .await?;
    sqlx::query(
        "INSERT INTO recording_reactions \
        (user_id,recording_id,reaction,changed_at) \
        SELECT f.user_id,?,f.reaction,f.changed_at FROM discovery_feedback f \
        LEFT JOIN recording_reaction_decisions d \
            ON d.user_id=f.user_id AND d.recording_id=? \
        WHERE f.candidate_id=? AND f.reaction IS NOT NULL \
            AND (d.changed_at IS NULL OR d.changed_at<f.changed_at) \
        ON CONFLICT(user_id,recording_id) DO UPDATE SET \
            reaction=excluded.reaction,changed_at=excluded.changed_at",
    )
    .bind(recording)
    .bind(recording)
    .bind(candidate)
    .execute(&mut *c)
    .await?;
    // Keep wishlist intent as a user's retained decision; live availability
    // changes independently and prevents unnecessary repeated downloads.
    Ok(())
}

/// Complete provenance and public job result in the same catalog transaction.
async fn provenance(
    c: &mut SqliteConnection,
    work: &Work,
    recording: i64,
    file: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO downloaded_sources VALUES (?,?,?) \
        ON CONFLICT(video_id) DO NOTHING",
    )
    .bind(&work.job.video_id)
    .bind(recording)
    .bind(file)
    .execute(&mut *c)
    .await?;
    sqlx::query(
        "UPDATE download_jobs SET status='completed',recording_id=?, \
        file_id=?,finished_at=unixepoch(),message=NULL WHERE id=?",
    )
    .bind(recording)
    .bind(file)
    .bind(work.job.id)
    .execute(c)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Source provenance, external feedback and durable explicit-clear
    //! semantics.
    use crate::jobs::downloads::tests::Fixture;

    /// Linking imports old external likes but respects newer native clears.
    #[tokio::test]
    async fn links_unverified_candidate_and_preserves_later_native_decisions()
    {
        let f = Fixture::new().await;
        let source = f.source().await;
        let job = f.service.download(1, source).await.unwrap();
        let done = f.wait(1, job.id).await;
        let recording = done.recording_id.unwrap();
        let suggestion = crate::db::discovery::tests::suggestion(
            "lastfm",
            "outside",
            "Unverified title",
            Some(crate::db::discovery::tests::MBID),
        );
        let candidate: i64 = sqlx::query_scalar(
            "INSERT INTO discovery_candidates \
            (identity_key,kind,mbid,document) \
            VALUES ('external','recording',?,?) RETURNING id",
        )
        .bind(&suggestion.candidate.mbid)
        .bind(serde_json::to_string(&suggestion.candidate).unwrap())
        .fetch_one(&f.pool)
        .await
        .unwrap();
        for user in [1, 2] {
            sqlx::query(
                "INSERT INTO discovery_feedback \
                (user_id,candidate_id,version,reaction,hidden, \
                    wanted,changed_at) \
                VALUES (?,?,1,'like',0,1,'2000-01-01T00:00:00.000Z')",
            )
            .bind(user)
            .bind(candidate)
            .execute(&f.pool)
            .await
            .unwrap();
        }
        crate::db::reactions::set(&f.pool, 2, recording, None)
            .await
            .unwrap();
        f.cool().await;
        let search = f
            .service
            .search(
                1,
                crate::downloads::SearchRequest::Candidate { id: candidate },
            )
            .await
            .unwrap();
        f.wait(1, search.id).await;
        let source = crate::db::downloads::sources(&f.pool, 1, search.id)
            .await
            .unwrap()[0]
            .id;
        let job = f.service.download(1, source).await.unwrap();
        assert_eq!(f.wait(1, job.id).await.recording_id, Some(recording));
        let alice = crate::db::discovery::candidate(&f.pool, 1, candidate)
            .await
            .unwrap();
        let bob = crate::db::discovery::candidate(&f.pool, 2, candidate)
            .await
            .unwrap();
        assert_eq!(alice.local_id, Some(recording));
        assert_eq!(alice.match_method.as_deref(), Some("download_selection"));
        assert_eq!(
            alice.reaction,
            Some(crate::library::reactions::Reaction::Like)
        );
        assert!(alice.has_registered_audio);
        assert!(alice.feedback.wanted);
        assert_eq!(bob.reaction, None);
        assert_eq!(
            crate::db::reactions::favorites(&f.pool, 1, 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            crate::db::reactions::favorites(&f.pool, 2, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        let mbid: Option<String> = sqlx::query_scalar(
            "SELECT mbid \
            FROM catalog_metadata WHERE entity_type='recording' \
            AND entity_id=?",
        )
        .bind(recording)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(mbid, None);
        f.service.close().await;
        f.pool.close().await;
    }

    /// Failed registration rolls back catalog rows; restart replays the
    /// object.
    #[tokio::test]
    async fn rolls_back_catalog_and_recovers_published_audio() {
        let f = Fixture::new().await;
        let source = f.source().await;
        sqlx::query(
            "CREATE TRIGGER reject_import BEFORE INSERT ON audio_files \
            BEGIN SELECT RAISE(ABORT,'private database detail'); END",
        )
        .execute(&f.pool)
        .await
        .unwrap();
        let job = f.service.download(1, source).await.unwrap();
        assert_eq!(f.wait(1, job.id).await.status, "failed");
        f.service.close().await;
        let (recordings, intents): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM recordings), \
                (SELECT count(*) FROM download_imports)",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!((recordings, intents), (0, 1));
        sqlx::query("DROP TRIGGER reject_import")
            .execute(&f.pool)
            .await
            .unwrap();
        let service = crate::jobs::downloads::DownloadService::new(
            f.pool.clone(),
            f.config.clone(),
            Some(f.storage.clone()),
        );
        service.recover().await.unwrap();
        assert_eq!(
            crate::db::downloads::job(&f.pool, 1, job.id)
                .await
                .unwrap()
                .status,
            "completed"
        );
        service.close().await;
        f.pool.close().await;
    }
}

#[cfg(test)]
mod policy_tests {
    //! Operator policy preserves explicit targets and prevents unwanted
    //! writes.
    use crate::{
        db,
        downloads::{SearchRequest, policy::DuplicatePolicy},
        jobs::downloads::{DownloadService, tests::Fixture},
    };

    /// Strict catalog creation requires a selected existing Recording.
    #[tokio::test]
    async fn creation_policy_rejects_unknown_tracks_before_provider_work() {
        let f = Fixture::new().await;
        let source = f.source().await;
        f.service.close().await;
        f.mode("fail");
        let mut config = f.config.clone();
        config.import.create_recordings = false;
        let service = DownloadService::new(
            f.pool.clone(),
            config,
            Some(f.storage.clone()),
        );
        let job = service.download(1, source).await.unwrap();
        let failed = f.wait(1, job.id).await;
        assert_eq!(failed.status, "failed");
        assert!(failed.message.unwrap().contains("existing recording"));
        let (recordings, journals): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM recordings), \
                (SELECT count(*) FROM download_imports)",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!((recordings, journals), (0, 0));
        let recording = db::recordings::create(
            &f.pool,
            crate::library::recordings::NewRecording::new("Curated", vec![])
                .unwrap(),
        )
        .await
        .unwrap();
        f.mode("ok");
        f.cool().await;
        let search = service
            .search(1, SearchRequest::Recording { id: recording.id })
            .await
            .unwrap();
        f.wait(1, search.id).await;
        let source =
            db::downloads::sources(&f.pool, 1, search.id).await.unwrap()[0].id;
        let job = service.download(1, source).await.unwrap();
        assert_eq!(f.wait(1, job.id).await.recording_id, Some(recording.id));
        service.close().await;
        f.pool.close().await;
    }

    /// Exact reuse and alternative-file admission are independent finite
    /// rules.
    #[tokio::test]
    async fn duplicate_and_alternative_policies_preserve_existing_catalog() {
        let f = Fixture::new().await;
        let source = f.source().await;
        let job = f.service.download(1, source).await.unwrap();
        let imported = f.wait(1, job.id).await;
        let recording = imported.recording_id.unwrap();
        f.service.close().await;
        f.mode("fail");
        let mut config = f.config.clone();
        config.import.duplicates = DuplicatePolicy::Reject;
        let reject = DownloadService::new(
            f.pool.clone(),
            config,
            Some(f.storage.clone()),
        );
        let job = reject.download(1, source).await.unwrap();
        let failed = f.wait(1, job.id).await;
        assert_eq!(failed.status, "failed");
        assert_eq!(
            failed.message.as_deref(),
            Some("Import policy rejects duplicates")
        );
        reject.close().await;
        let mut config = f.config.clone();
        config.import.allow_alternate_files = false;
        let strict = DownloadService::new(
            f.pool.clone(),
            config,
            Some(f.storage.clone()),
        );
        let job = strict.download(1, source).await.unwrap();
        assert_eq!(f.wait(1, job.id).await.file_id, imported.file_id);
        f.cool().await;
        let search = strict
            .search(1, SearchRequest::Recording { id: recording })
            .await
            .unwrap();
        f.wait(1, search.id).await;
        let source: i64 = sqlx::query_scalar(
            "INSERT INTO download_sources (job_id,video_id,title) \
            VALUES (?,'01234567890','Another upload') RETURNING id",
        )
        .bind(search.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        let job = strict.download(1, source).await.unwrap();
        let failed = f.wait(1, job.id).await;
        assert_eq!(failed.status, "failed");
        assert!(failed.message.unwrap().contains("alternative files"));
        let (recordings, files): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM recordings), \
                (SELECT count(*) FROM audio_files)",
        )
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!((recordings, files), (1, 1));
        strict.close().await;
        f.pool.close().await;
    }
}
