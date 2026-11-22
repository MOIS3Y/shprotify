//! Atomic track placement and ordered release tracklists in SQLite.

use sqlx::SqlitePool;

use crate::library::release_tracks::{NewReleaseTrack, ReleaseTrack};

/// Expected placement failures are distinct from unexpected database errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TrackError {
    /// The parent release does not exist.
    #[error("Release does not exist")]
    MissingRelease,
    /// The referenced recording does not exist.
    #[error("Recording does not exist")]
    MissingRecording,
    /// Another track occupies this release, disc, and position.
    #[error("Track position is already occupied")]
    PositionOccupied,
    /// Unexpected storage failure, reserved for server diagnostics.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Insert a track with reference and uniqueness checks in one statement.
///
/// Checking the parent inside INSERT avoids a race between existence checks
/// and writes. Concurrent requests cannot occupy the same disc/position.
pub(crate) async fn create(
    pool: &SqlitePool,
    release_id: i64,
    input: NewReleaseTrack,
) -> Result<ReleaseTrack, TrackError> {
    let result = sqlx::query_as(
        "INSERT INTO release_tracks \
         (release_id, recording_id, disc_number, position) \
         SELECT ?, ?, ?, ? WHERE EXISTS \
         (SELECT 1 FROM releases WHERE id = ?) \
         RETURNING id, release_id, recording_id, disc_number, position",
    )
    .bind(release_id)
    .bind(input.recording_id)
    .bind(input.disc_number)
    .bind(input.position)
    .bind(release_id)
    .fetch_optional(pool)
    .await;
    match result {
        Ok(Some(track)) => Ok(track),
        Ok(None) => Err(TrackError::MissingRelease),
        Err(error) => {
            if let Some(cause) = error.as_database_error() {
                if cause.is_unique_violation() {
                    return Err(TrackError::PositionOccupied);
                }
                if cause.is_foreign_key_violation() {
                    return Err(TrackError::MissingRecording);
                }
            }
            Err(error.into())
        }
    }
}

/// Fetch an occurrence only if it belongs to the specified parent release.
pub(crate) async fn find(
    pool: &SqlitePool,
    release_id: i64,
    track_id: i64,
) -> Result<Option<ReleaseTrack>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, release_id, recording_id, disc_number, position \
         FROM release_tracks WHERE release_id = ? AND id = ?",
    )
    .bind(release_id)
    .bind(track_id)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Base-table reads used by migration and import fixtures.
    use super::*;

    /// Read original catalog rows independently of client projections.
    pub(crate) async fn list(
        pool: &SqlitePool,
        release_id: i64,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ReleaseTrack>, TrackError> {
        let mut transaction = pool.begin().await?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM releases WHERE id = ?)",
        )
        .bind(release_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !exists {
            return Err(TrackError::MissingRelease);
        }
        let tracks = sqlx::query_as(
            "SELECT id, release_id, recording_id, disc_number, position \
         FROM release_tracks WHERE release_id = ? \
         ORDER BY disc_number, position LIMIT ? OFFSET ?",
        )
        .bind(release_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(tracks)
    }
}

#[cfg(test)]
mod tests {
    //! Track ordering, concurrent conflicts, and reference integrity.

    use crate::library::{
        recordings::NewRecording,
        releases::{NewRelease, ReleaseKind},
    };
    use tempfile::TempDir;

    use super::fixtures::list;
    use super::*;

    /// Seed independent release and recording identities in an isolated file.
    async fn setup() -> (TempDir, SqlitePool, i64, i64) {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let release = crate::db::releases::create(
            &pool,
            NewRelease::new("Album", ReleaseKind::Album, vec![]).unwrap(),
        )
        .await
        .unwrap();
        let recording = crate::db::recordings::create(
            &pool,
            NewRecording::new("Recording", vec![]).unwrap(),
        )
        .await
        .unwrap();
        (directory, pool, release.id, recording.id)
    }

    /// Reused recordings keep independent placements, sorted by disc/position.
    #[tokio::test]
    async fn persists_reused_recordings_and_orders_track_pages() {
        let (directory, pool, release, recording) = setup().await;
        let mut ids = Vec::new();
        for (disc, position) in [(2, 1), (1, 3), (1, 1)] {
            ids.push(
                create(
                    &pool,
                    release,
                    NewReleaseTrack::new(recording, disc, position).unwrap(),
                )
                .await
                .unwrap()
                .id,
            );
        }
        let other = crate::db::releases::create(
            &pool,
            NewRelease::new("Other", ReleaseKind::Album, vec![]).unwrap(),
        )
        .await
        .unwrap();
        create(
            &pool,
            other.id,
            NewReleaseTrack::new(recording, 1, 1).unwrap(),
        )
        .await
        .unwrap();
        assert!(find(&pool, other.id, ids[0]).await.unwrap().is_none());
        pool.close().await;

        let pool = crate::db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let tracks = list(&pool, release, 50, 0).await.unwrap();
        assert_eq!(
            tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            [ids[2], ids[1], ids[0]]
        );
        assert_eq!(list(&pool, release, 1, 1).await.unwrap()[0].id, ids[1]);
        assert!(list(&pool, release, 50, 3).await.unwrap().is_empty());
        assert_eq!(
            find(&pool, release, ids[0])
                .await
                .unwrap()
                .unwrap()
                .recording_id,
            recording
        );
        pool.close().await;
    }

    /// Exactly one of two competing inserts can claim a track position.
    #[tokio::test]
    async fn concurrent_position_conflicts_do_not_overwrite_tracks() {
        let (_directory, pool, release, recording) = setup().await;
        let (first, second) = tokio::join!(
            create(
                &pool,
                release,
                NewReleaseTrack::new(recording, 1, 1).unwrap()
            ),
            create(
                &pool,
                release,
                NewReleaseTrack::new(recording, 1, 1).unwrap()
            ),
        );
        let outcomes = [first, second];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(
                    result,
                    Err(TrackError::PositionOccupied)
                ))
                .count(),
            1
        );
        let tracks = list(&pool, release, 50, 0).await.unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].recording_id, recording);
        pool.close().await;
    }

    /// Missing references insert no rows; empty existing releases stay valid.
    #[tokio::test]
    async fn missing_references_leave_no_partial_tracks() {
        let (_directory, pool, release, recording) = setup().await;
        assert!(list(&pool, release, 50, 0).await.unwrap().is_empty());
        assert!(matches!(
            create(
                &pool,
                release + 1,
                NewReleaseTrack::new(recording, 1, 1).unwrap()
            )
            .await,
            Err(TrackError::MissingRelease)
        ));
        assert!(matches!(
            create(
                &pool,
                release,
                NewReleaseTrack::new(recording + 1, 1, 1).unwrap()
            )
            .await,
            Err(TrackError::MissingRecording)
        ));
        assert!(matches!(
            list(&pool, release + 1, 50, 0).await,
            Err(TrackError::MissingRelease)
        ));
        assert!(list(&pool, release, 50, 0).await.unwrap().is_empty());
        pool.close().await;
    }

    /// Release deletion removes placements while recording identity survives.
    #[tokio::test]
    async fn foreign_keys_protect_recordings_and_cascade_release_tracks() {
        let (_directory, pool, release, recording) = setup().await;
        create(
            &pool,
            release,
            NewReleaseTrack::new(recording, 1, 1).unwrap(),
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
        sqlx::query("DELETE FROM releases WHERE id = ?")
            .bind(release)
            .execute(&pool)
            .await
            .unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM release_tracks")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
        assert!(
            crate::db::recordings::find(&pool, recording)
                .await
                .unwrap()
                .is_some()
        );
        pool.close().await;
    }
}
