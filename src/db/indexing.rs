//! Atomic file indexing with conservative, directory-local tag associations.
//!
//! New paths get independent recordings. Existing paths retain all catalog
//! identities and curated metadata; only measured file properties and source
//! tags are refreshed. Missing files are never removed by this importer.
//! New recordings receive local source associations through `matching`.

use std::path::Path;

use sqlx::SqlitePool;

use crate::library::{
    audio_files::NewAudioFile,
    file_state::FileState,
    metadata::{AudioTags, ScannedFile},
    recordings::NewRecording,
};

/// Invalid source metadata can be isolated; storage failures stop the job.
#[derive(Debug, thiserror::Error)]
pub(crate) enum IndexError {
    /// Source values cannot be represented in the catalog.
    #[error("{0}")]
    Invalid(&'static str),
    /// Database error; detailed diagnostics are reserved for server logs.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
    /// Unexpected tag serialization error.
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

/// Index one observation and increment the owning job counter atomically.
///
/// Write reservation precedes reads, avoiding SQLite read-to-write races with
/// manual registration. A failed import rolls back recordings, files, tags,
/// and counters together. Valid existing paths do not revalidate a title that
/// will not be used, preserving manual corrections even with poor source tags.
pub(crate) async fn store(
    pool: &SqlitePool,
    scan_id: i64,
    source: ScannedFile,
) -> Result<(), IndexError> {
    let tags = serde_json::to_string(&source.tags)?;
    let input =
        NewAudioFile::new(&source.path, &source.format, source.properties)
            .map_err(IndexError::Invalid)?;
    let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM audio_files WHERE path = ?")
            .bind(&input.path)
            .fetch_optional(&mut *transaction)
            .await?;
    let imported = existing.is_none();
    let file_id = match existing {
        Some(id) => id,
        None => {
            let fallback = Path::new(&input.path)
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled");
            let title = source
                .tags
                .title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(fallback);
            let recording = NewRecording::new(title, vec![])
                .map_err(IndexError::Invalid)?;
            let recording_id: i64 = sqlx::query_scalar(
                "INSERT INTO recordings (title) VALUES (?) RETURNING id",
            )
            .bind(recording.title)
            .fetch_one(&mut *transaction)
            .await?;
            if let Some(message) = super::matching::attach(
                &mut transaction,
                recording_id,
                &input.path,
                &source.tags,
            )
            .await?
            {
                super::scans::record_error_in(
                    &mut transaction,
                    scan_id,
                    &input.path,
                    message,
                )
                .await?;
            }
            sqlx::query_scalar(
                "INSERT INTO audio_files (recording_id, path, format) \
                 VALUES (?, ?, ?) RETURNING id",
            )
            .bind(recording_id)
            .bind(&input.path)
            .bind(&input.format)
            .fetch_one(&mut *transaction)
            .await?
        }
    };
    let p = input.properties;
    sqlx::query(
        "UPDATE audio_files SET format = ?, size_bytes = ?, duration_ms = ?, \
         sample_rate_hz = ?, channels = ?, bit_depth = ?, bitrate_bps = ? \
         WHERE id = ?",
    )
    .bind(input.format)
    .bind(p.size_bytes)
    .bind(p.duration_ms)
    .bind(p.sample_rate_hz)
    .bind(p.channels)
    .bind(p.bit_depth)
    .bind(p.bitrate_bps)
    .bind(file_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO audio_file_tags (file_id, tags) VALUES (?, ?) \
         ON CONFLICT(file_id) DO UPDATE SET tags = excluded.tags",
    )
    .bind(file_id)
    .bind(tags)
    .execute(&mut *transaction)
    .await?;
    if let Some(state) = source.state {
        sqlx::query(
            "INSERT INTO audio_file_states (file_id, state) \
            VALUES (?, ?) ON CONFLICT(file_id) \
            DO UPDATE SET state = excluded.state",
        )
        .bind(file_id)
        .bind(serde_json::to_string(&state)?)
        .execute(&mut *transaction)
        .await?;
    } else {
        sqlx::query("DELETE FROM audio_file_states WHERE file_id = ?")
            .bind(file_id)
            .execute(&mut *transaction)
            .await?;
    }
    if let Some(digest) = source.sha256 {
        sqlx::query(
            "INSERT INTO audio_file_hashes (file_id, sha256) \
            VALUES (?, ?) ON CONFLICT(file_id) \
            DO UPDATE SET sha256 = excluded.sha256",
        )
        .bind(file_id)
        .bind(digest)
        .execute(&mut *transaction)
        .await?;
    } else {
        sqlx::query("DELETE FROM audio_file_hashes WHERE file_id = ?")
            .bind(file_id)
            .execute(&mut *transaction)
            .await?;
    }
    let updated = sqlx::query(
        "UPDATE library_scans SET imported = imported + ?, \
         refreshed = refreshed + ? WHERE id = ? AND status = 'running'",
    )
    .bind(i64::from(imported))
    .bind(i64::from(!imported))
    .bind(scan_id)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(IndexError::Invalid("Scan is no longer running"));
    }
    transaction.commit().await?;
    Ok(())
}

/// Compare a current filesystem observation to the last committed audio probe.
///
/// Legacy/manual files and failed imports have no snapshot and must be probed.
/// This is a bounded lookup by the existing globally unique file path.
pub(crate) async fn unchanged(
    pool: &SqlitePool,
    path: &str,
    state: &FileState,
) -> Result<bool, IndexError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM audio_file_states s \
         JOIN audio_files f ON f.id = s.file_id \
         WHERE f.path = ? AND s.state = ?)",
    )
    .bind(path)
    .bind(serde_json::to_string(state)?)
    .fetch_one(pool)
    .await?)
}

/// Fetch the latest source tags, scoped to the recording and physical file.
///
/// Manual registrations have no source observation and return `None` until a
/// scan probes the file. Source tags are not treated as curated artist IDs.
pub(crate) async fn tags(
    pool: &SqlitePool,
    recording_id: i64,
    file_id: i64,
) -> anyhow::Result<Option<AudioTags>> {
    let tags: Option<String> = sqlx::query_scalar(
        "SELECT t.tags FROM audio_file_tags t \
         JOIN audio_files f ON f.id = t.file_id \
         WHERE f.id = ? AND f.recording_id = ?",
    )
    .bind(file_id)
    .bind(recording_id)
    .fetch_optional(pool)
    .await?;
    Ok(tags.map(|json| serde_json::from_str(&json)).transpose()?)
}

#[cfg(test)]
mod tests {
    //! Path identity, curated metadata preservation, and transaction rollback.

    use super::*;
    use crate::{db, library::audio_files::AudioProperties};

    /// Build source observations independently of filesystem/probe behavior.
    fn source(path: &str, title: Option<&str>) -> ScannedFile {
        ScannedFile {
            path: path.into(),
            state: None,
            sha256: None,
            format: "flac".into(),
            tags: AudioTags {
                title: title.map(str::to_owned),
                artist: Some("Same credit".into()),
                album: Some("Same album".into()),
                ..AudioTags::default()
            },
            properties: AudioProperties {
                duration_ms: Some(1000),
                ..AudioProperties::default()
            },
        }
    }

    /// Equal titles keep recordings distinct while local credits are reused.
    #[tokio::test]
    async fn indexes_distinct_paths_and_exposes_scoped_source_tags() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        for path in ["/music/one.flac", "/music/two.flac"] {
            store(&pool, job.id, source(path, Some(" Title ")))
                .await
                .unwrap();
        }
        let rows: Vec<(i64, i64, String)> = sqlx::query_as(
            "SELECT f.id, r.id, r.title FROM audio_files f \
             JOIN recordings r ON r.id = f.recording_id ORDER BY f.id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].1, rows[1].1);
        assert_eq!(rows[0].2, "Title");
        let tags = tags(&pool, rows[0].1, rows[0].0).await.unwrap().unwrap();
        assert_eq!(tags.title.as_deref(), Some(" Title "));
        assert_eq!(tags.artist.as_deref(), Some("Same credit"));
        assert!(
            super::tags(&pool, rows[1].1, rows[0].0)
                .await
                .unwrap()
                .is_none()
        );
        let artists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artists")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(artists, 1);
        let job = db::scans::find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(job.imported, 2);
        assert_eq!(job.refreshed, 0);
        pool.close().await;
    }

    /// Existing manual identities and credits survive new source observations.
    #[tokio::test]
    async fn refreshes_manual_files_without_overwriting_catalog_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO artists (id, name) VALUES (1, 'Curated')")
            .execute(&pool)
            .await
            .unwrap();
        let recording = db::recordings::create(
            &pool,
            NewRecording::new("Manual title", vec![1]).unwrap(),
        )
        .await
        .unwrap();
        let file = db::audio_files::create(
            &pool,
            recording.id,
            NewAudioFile::new(
                "/music/one.flac",
                "mp3",
                AudioProperties::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(tags(&pool, recording.id, file.id).await.unwrap().is_none());
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        for title in [Some("Changed"), Some("invalid\nsource title")] {
            store(&pool, job.id, source("/music/one.flac", title))
                .await
                .unwrap();
        }
        let found = db::recordings::find(&pool, recording.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.title, "Manual title");
        assert_eq!(found.artist_ids, [1]);
        let found = db::audio_files::find(&pool, recording.id, file.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.format, "flac");
        assert_eq!(found.properties.duration_ms, Some(1000));
        let job = db::scans::find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(job.imported, 0);
        assert_eq!(job.refreshed, 2);
        pool.close().await;
    }

    /// Missing/blank titles fall back to the filename; invalid titles reject.
    #[tokio::test]
    async fn falls_back_to_filename_and_rejects_invalid_new_titles() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        store(&pool, job.id, source("/music/Unknown track.flac", None))
            .await
            .unwrap();
        let title: String = sqlx::query_scalar("SELECT title FROM recordings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(title, "Unknown track");
        assert!(matches!(
            store(
                &pool,
                job.id,
                source("/music/rejected.flac", Some(&"a".repeat(256)))
            )
            .await,
            Err(IndexError::Invalid(_))
        ));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        pool.close().await;
    }

    /// Failure after file creation leaves no partial catalog rows or progress.
    #[tokio::test]
    async fn rolls_back_catalog_tags_and_progress_on_storage_failure() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_tags BEFORE INSERT \
            ON audio_file_tags BEGIN SELECT RAISE(ABORT, 'secret'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            store(&pool, job.id, source("/music/one.flac", Some("Title")))
                .await,
            Err(IndexError::Storage(_))
        ));
        for sql in [
            "SELECT COUNT(*) FROM recordings",
            "SELECT COUNT(*) FROM audio_files",
            "SELECT COUNT(*) FROM audio_file_tags",
            "SELECT COUNT(*) FROM audio_file_states",
            "SELECT COUNT(*) FROM artists",
            "SELECT COUNT(*) FROM releases",
            "SELECT COUNT(*) FROM scan_artist_matches",
            "SELECT COUNT(*) FROM scan_release_matches",
        ] {
            let count: i64 =
                sqlx::query_scalar(sql).fetch_one(&pool).await.unwrap();
            assert_eq!(count, 0);
        }
        assert_eq!(
            db::scans::find(&pool, job.id)
                .await
                .unwrap()
                .unwrap()
                .imported,
            0
        );
        pool.close().await;
    }

    /// Concurrent observations of one path produce one recording and file.
    #[tokio::test]
    async fn concurrent_imports_preserve_path_identity() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        let (first, second) = tokio::join!(
            store(&pool, job.id, source("/music/one.flac", Some("One"))),
            store(&pool, job.id, source("/music/one.flac", Some("Two"))),
        );
        first.unwrap();
        second.unwrap();
        let job = db::scans::find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(job.imported, 1);
        assert_eq!(job.refreshed, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        pool.close().await;
    }

    /// Probe observations commit with catalog changes and survive reopening.
    #[tokio::test]
    async fn snapshots_are_atomic_and_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("song.wav");
        std::fs::write(&path, crate::library::metadata::tests::wave())
            .unwrap();
        let database = directory.path().join("catalog.db");
        let pool = db::connect(&database).await.unwrap();
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        let state = FileState::read(&path).unwrap();
        let name = path.to_str().unwrap();
        assert!(!unchanged(&pool, name, &state).await.unwrap());
        sqlx::query(
            "CREATE TRIGGER reject_snapshot BEFORE INSERT \
            ON audio_file_states BEGIN SELECT RAISE(ABORT, 'failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let observed = crate::library::metadata::read(&path).unwrap().unwrap();
        assert!(matches!(
            store(&pool, job.id, observed).await,
            Err(IndexError::Storage(_))
        ));
        assert!(!unchanged(&pool, name, &state).await.unwrap());
        let rows: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM audio_files), \
             (SELECT COUNT(*) FROM audio_file_tags)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, (0, 0));
        sqlx::query("DROP TRIGGER reject_snapshot")
            .execute(&pool)
            .await
            .unwrap();
        store(
            &pool,
            job.id,
            crate::library::metadata::read(&path).unwrap().unwrap(),
        )
        .await
        .unwrap();
        assert!(unchanged(&pool, name, &state).await.unwrap());
        pool.close().await;
        let pool = db::connect(&database).await.unwrap();
        assert!(unchanged(&pool, name, &state).await.unwrap());
        let job = db::scans::find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(job.imported, 1);
        pool.close().await;
    }

    /// Probe errors invalidate snapshots even if filesystem metadata matches.
    #[tokio::test]
    async fn failed_probes_and_unverified_updates_invalidate_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("song.wav");
        std::fs::write(&path, crate::library::metadata::tests::wave())
            .unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        let state = FileState::read(&path).unwrap();
        let name = path.to_str().unwrap();
        store(
            &pool,
            job.id,
            crate::library::metadata::read(&path).unwrap().unwrap(),
        )
        .await
        .unwrap();
        db::scans::record_error(&pool, job.id, name, "Probe failed")
            .await
            .unwrap();
        assert!(!unchanged(&pool, name, &state).await.unwrap());
        store(
            &pool,
            job.id,
            crate::library::metadata::read(&path).unwrap().unwrap(),
        )
        .await
        .unwrap();
        let mut unverified =
            crate::library::metadata::read(&path).unwrap().unwrap();
        unverified.state = None;
        store(&pool, job.id, unverified).await.unwrap();
        assert!(!unchanged(&pool, name, &state).await.unwrap());
        let tags: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audio_file_tags")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tags, 1);
        pool.close().await;
    }
}
