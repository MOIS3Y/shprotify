//! Recording file registration, global path identity, and bounded reads.

use sqlx::SqlitePool;

use crate::library::audio_files::{AudioFile, NewAudioFile};

/// Expected catalog failures are distinct from unexpected storage errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FileError {
    /// The parent recording does not exist.
    #[error("Recording does not exist")]
    MissingRecording,
    /// The normalized path is already assigned to a catalog file.
    #[error("File path is already registered")]
    PathConflict,
    /// Unexpected storage failure, reserved for server logs.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Register validated metadata atomically without opening the physical file.
///
/// The parent check and global path uniqueness are enforced in one statement,
/// including concurrent registrations against different recordings.
pub(crate) async fn create(
    pool: &SqlitePool,
    recording_id: i64,
    input: NewAudioFile,
) -> Result<AudioFile, FileError> {
    let properties = input.properties;
    let result = sqlx::query_as(
        "INSERT INTO audio_files \
         (recording_id, path, format, size_bytes, duration_ms, \
          sample_rate_hz, channels, bit_depth, bitrate_bps) \
         SELECT ?, ?, ?, ?, ?, ?, ?, ?, ? WHERE EXISTS \
         (SELECT 1 FROM recordings WHERE id = ?) \
         RETURNING id, recording_id, path, format, size_bytes, duration_ms, \
                   sample_rate_hz, channels, bit_depth, bitrate_bps",
    )
    .bind(recording_id)
    .bind(input.path)
    .bind(input.format)
    .bind(properties.size_bytes)
    .bind(properties.duration_ms)
    .bind(properties.sample_rate_hz)
    .bind(properties.channels)
    .bind(properties.bit_depth)
    .bind(properties.bitrate_bps)
    .bind(recording_id)
    .fetch_optional(pool)
    .await;
    match result {
        Ok(Some(file)) => Ok(file),
        Ok(None) => Err(FileError::MissingRecording),
        Err(error) => {
            if error
                .as_database_error()
                .is_some_and(|cause| cause.is_unique_violation())
            {
                return Err(FileError::PathConflict);
            }
            Err(error.into())
        }
    }
}

/// Read metadata only if this file belongs to the requested recording.
pub(crate) async fn find(
    pool: &SqlitePool,
    recording_id: i64,
    file_id: i64,
) -> Result<Option<AudioFile>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, recording_id, path, format, size_bytes, duration_ms, \
                sample_rate_hz, channels, bit_depth, bitrate_bps \
         FROM audio_files WHERE recording_id = ? AND id = ? \
         AND NOT EXISTS(SELECT 1 FROM file_removals d \
             WHERE d.file_id=audio_files.id AND d.status='pending')",
    )
    .bind(recording_id)
    .bind(file_id)
    .fetch_optional(pool)
    .await
}

/// Read one recording's metadata page in a consistent database snapshot.
///
/// Missing recordings are errors; existing recordings without files yield an
/// empty page. Callers validate limits and offsets before entering this code.
pub(crate) async fn list(
    pool: &SqlitePool,
    recording_id: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<AudioFile>, FileError> {
    let mut transaction = pool.begin().await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM recordings WHERE id = ?)",
    )
    .bind(recording_id)
    .fetch_one(&mut *transaction)
    .await?;
    if !exists {
        return Err(FileError::MissingRecording);
    }
    let files = sqlx::query_as(
        "SELECT id, recording_id, path, format, size_bytes, duration_ms, \
                sample_rate_hz, channels, bit_depth, bitrate_bps \
         FROM audio_files WHERE recording_id = ? \
         AND NOT EXISTS(SELECT 1 FROM file_removals d \
             WHERE d.file_id=audio_files.id AND d.status='pending') \
         ORDER BY id LIMIT ? OFFSET ?",
    )
    .bind(recording_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    //! Metadata round-trips, global path conflicts, and reference integrity.

    use crate::library::{
        audio_files::AudioProperties, recordings::NewRecording,
    };

    use super::*;

    /// Seed one independent recording in a migrated temporary database.
    async fn setup() -> (tempfile::TempDir, SqlitePool, i64) {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let recording = crate::db::recordings::create(
            &pool,
            NewRecording::new("Recording", vec![]).unwrap(),
        )
        .await
        .unwrap();
        (directory, pool, recording.id)
    }

    /// Files of different formats persist without reading or modifying bytes.
    #[tokio::test]
    async fn persists_multiple_formats_and_declared_properties() {
        let (directory, pool, recording) = setup().await;
        let physical = directory.path().join("track.flac");
        std::fs::write(&physical, b"untouched bytes").unwrap();
        let file = create(
            &pool,
            recording,
            NewAudioFile::new(
                physical.to_str().unwrap(),
                "FLAC",
                AudioProperties {
                    size_bytes: Some(15),
                    duration_ms: Some(1234),
                    sample_rate_hz: Some(44100),
                    channels: Some(2),
                    bit_depth: Some(16),
                    bitrate_bps: Some(900000),
                },
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let absent = directory.path().join("track.mp3");
        let other = create(
            &pool,
            recording,
            NewAudioFile::new(
                absent.to_str().unwrap(),
                "mp3",
                AudioProperties::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(!absent.exists());
        assert_eq!(std::fs::read(&physical).unwrap(), b"untouched bytes");
        pool.close().await;

        let pool = crate::db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let stored = find(&pool, recording, file.id).await.unwrap().unwrap();
        assert_eq!(stored.format, "flac");
        assert_eq!(stored.properties.size_bytes, Some(15));
        assert_eq!(stored.properties.duration_ms, Some(1234));
        assert_eq!(stored.properties.sample_rate_hz, Some(44100));
        assert_eq!(stored.properties.channels, Some(2));
        assert_eq!(stored.properties.bit_depth, Some(16));
        assert_eq!(stored.properties.bitrate_bps, Some(900000));
        let page = list(&pool, recording, 1, 1).await.unwrap();
        assert_eq!(page[0].id, other.id);
        assert!(page[0].properties.bit_depth.is_none());
        assert!(list(&pool, recording, 50, 2).await.unwrap().is_empty());
        pool.close().await;
    }

    /// Lexical aliases and other recordings cannot claim an existing path.
    #[tokio::test]
    async fn normalized_paths_are_unique_across_recordings() {
        let (_directory, pool, recording) = setup().await;
        let file = create(
            &pool,
            recording,
            NewAudioFile::new(
                "/music/./track.flac",
                "flac",
                AudioProperties::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let other = crate::db::recordings::create(
            &pool,
            NewRecording::new("Other", vec![]).unwrap(),
        )
        .await
        .unwrap();
        for parent in [recording, other.id] {
            let error = create(
                &pool,
                parent,
                NewAudioFile::new(
                    "/music//track.flac",
                    "mp3",
                    AudioProperties::default(),
                )
                .unwrap(),
            )
            .await
            .unwrap_err();
            assert!(matches!(error, FileError::PathConflict));
        }
        assert!(find(&pool, other.id, file.id).await.unwrap().is_none());
        let stored = find(&pool, recording, file.id).await.unwrap().unwrap();
        assert_eq!(stored.format, "flac");
        assert!(list(&pool, other.id, 50, 0).await.unwrap().is_empty());
        pool.close().await;
    }

    /// Concurrent registrations preserve one owner and one metadata row.
    #[tokio::test]
    async fn concurrent_path_registration_has_one_winner() {
        let (_directory, pool, recording) = setup().await;
        let first = NewAudioFile::new(
            "/music/track.flac",
            "flac",
            AudioProperties::default(),
        )
        .unwrap();
        let second = NewAudioFile::new(
            "/music/track.flac",
            "flac",
            AudioProperties::default(),
        )
        .unwrap();
        let (first, second) = tokio::join!(
            create(&pool, recording, first),
            create(&pool, recording, second),
        );
        let outcomes = [first, second];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(FileError::PathConflict)
                ))
                .count(),
            1
        );
        assert_eq!(list(&pool, recording, 50, 0).await.unwrap().len(), 1);
        pool.close().await;
    }

    /// Missing parents insert nothing and referenced recordings are protected.
    #[tokio::test]
    async fn missing_parents_and_foreign_keys_preserve_catalog_integrity() {
        let (_directory, pool, recording) = setup().await;
        assert!(list(&pool, recording, 50, 0).await.unwrap().is_empty());
        assert!(matches!(
            create(
                &pool,
                recording + 1,
                NewAudioFile::new(
                    "/music/missing.flac",
                    "flac",
                    AudioProperties::default()
                )
                .unwrap()
            )
            .await,
            Err(FileError::MissingRecording)
        ));
        assert!(matches!(
            list(&pool, recording + 1, 50, 0).await,
            Err(FileError::MissingRecording)
        ));
        let file = create(
            &pool,
            recording,
            NewAudioFile::new(
                "/music/existing.flac",
                "flac",
                AudioProperties::default(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert!(
            sqlx::query("DELETE FROM recordings WHERE id = ?")
                .bind(recording)
                .execute(&pool)
                .await
                .is_err()
        );
        assert_eq!(list(&pool, recording, 50, 0).await.unwrap().len(), 1);
        assert!(find(&pool, recording, file.id + 1).await.unwrap().is_none());
        pool.close().await;
    }
}
