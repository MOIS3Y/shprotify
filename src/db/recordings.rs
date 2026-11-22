//! Atomic recording creation and ordered catalog reads in SQLite.

use sqlx::SqlitePool;

use crate::library::recordings::{NewRecording, Recording};

/// Creation failures distinguish unknown credited artists from storage errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CreateRecordingError {
    /// A credited artist does not exist; the transaction is rolled back.
    #[error("A credited artist does not exist")]
    UnknownArtist,
    /// Unexpected SQL failure, reserved for server-side diagnostics.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Insert the recording and ordered credits in one transaction.
///
/// SQLx rolls back an uncommitted transaction on early return, including when
/// a later credit fails after earlier credits were inserted successfully.
pub(crate) async fn create(
    pool: &SqlitePool,
    input: NewRecording,
) -> Result<Recording, CreateRecordingError> {
    let mut transaction = pool.begin().await?;
    let id = sqlx::query_scalar(
        "INSERT INTO recordings (title) VALUES (?) RETURNING id",
    )
    .bind(&input.title)
    .fetch_one(&mut *transaction)
    .await?;
    for (artist_id, position) in input.artist_ids.iter().zip(0_i64..) {
        let result = sqlx::query(
            "INSERT INTO recording_artists \
             (recording_id, artist_id, position) VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(artist_id)
        .bind(position)
        .execute(&mut *transaction)
        .await;
        if let Err(error) = result {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_foreign_key_violation())
            {
                return Err(CreateRecordingError::UnknownArtist);
            }
            return Err(error.into());
        }
    }
    transaction.commit().await?;
    Ok(Recording {
        id,
        title: input.title,
        artist_ids: input.artist_ids,
    })
}

/// One row per joined credit; uncredited recordings have a null artist ID.
#[derive(sqlx::FromRow)]
struct RecordingRow {
    /// Recording identifier used to group consecutive joined rows.
    id: i64,
    /// Stored display title.
    title: String,
    /// One credited artist, if present.
    artist_id: Option<i64>,
}

/// Reassemble ordered join rows without issuing one query per recording.
fn collect_recordings(rows: Vec<RecordingRow>) -> Vec<Recording> {
    let mut recordings: Vec<Recording> = Vec::new();
    for row in rows {
        if let Some(last) = recordings.last_mut().filter(|r| r.id == row.id) {
            last.artist_ids.extend(row.artist_id);
            continue;
        }
        recordings.push(Recording {
            id: row.id,
            title: row.title,
            artist_ids: row.artist_id.into_iter().collect(),
        });
    }
    recordings
}

/// Fetch one recording and its ordered credits in a single consistent query.
pub(crate) async fn find(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<Recording>, sqlx::Error> {
    let rows = sqlx::query_as(
        "SELECT r.id, r.title, a.artist_id FROM recordings r \
         LEFT JOIN recording_artists a ON a.recording_id = r.id \
         WHERE r.id = ? ORDER BY a.position",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(collect_recordings(rows).into_iter().next())
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Base-table reads used by migration and import fixtures.
    use super::*;

    /// Read original catalog rows independently of client projections.
    pub(crate) async fn list(
        pool: &SqlitePool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Recording>, sqlx::Error> {
        let rows = sqlx::query_as(
            "WITH page AS (SELECT id, title FROM recordings \
             ORDER BY id LIMIT ? OFFSET ?) \
         SELECT p.id, p.title, a.artist_id FROM page p \
         LEFT JOIN recording_artists a ON a.recording_id = p.id \
         ORDER BY p.id, a.position",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        Ok(collect_recordings(rows))
    }
}

#[cfg(test)]
mod tests {
    //! Recording independence, credit integrity, and atomic persistence.

    use crate::library::ArtistName;

    use super::fixtures::list;
    use super::*;

    /// Create an artist used by ordered recording credits.
    async fn artist(pool: &SqlitePool, name: &str) -> i64 {
        crate::db::artists::create(pool, &ArtistName::parse(name).unwrap())
            .await
            .unwrap()
            .id
    }

    /// Recordings persist without releases and pages include complete credits.
    #[tokio::test]
    async fn persists_independent_recordings_with_ordered_credits() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.db");
        let pool = crate::db::connect(&path).await.unwrap();
        let first = artist(&pool, "First").await;
        let second = artist(&pool, "Second").await;
        let recording = create(
            &pool,
            NewRecording::new("Same", vec![second, first]).unwrap(),
        )
        .await
        .unwrap();
        let alternate =
            create(&pool, NewRecording::new("Same", vec![]).unwrap())
                .await
                .unwrap();
        assert_ne!(recording.id, alternate.id);
        let releases: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM releases")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(releases, 0);
        pool.close().await;

        let pool = crate::db::connect(&path).await.unwrap();
        let fetched = find(&pool, recording.id).await.unwrap().unwrap();
        assert_eq!(fetched.artist_ids, [second, first]);
        assert_eq!(fetched.title, "Same");
        let page = list(&pool, 1, 0).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].artist_ids, [second, first]);
        let page = list(&pool, 1, 1).await.unwrap();
        assert_eq!(page[0].id, alternate.id);
        assert!(page[0].artist_ids.is_empty());
        assert!(list(&pool, 50, 2).await.unwrap().is_empty());
        assert!(find(&pool, alternate.id + 1).await.unwrap().is_none());
        pool.close().await;
    }

    /// Missing later credits roll back both the recording and earlier credits.
    #[tokio::test]
    async fn unknown_artist_rolls_back_the_entire_recording() {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let id = artist(&pool, "Existing").await;
        let error = create(
            &pool,
            NewRecording::new("Invalid", vec![id, id + 1]).unwrap(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, CreateRecordingError::UnknownArtist));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
            .fetch_one(&pool)
            .await
            .unwrap();
        let credits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recording_artists")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((count, credits), (0, 0));
        let valid =
            create(&pool, NewRecording::new("Valid", vec![id]).unwrap())
                .await
                .unwrap();
        assert_eq!(valid.artist_ids, [id]);
        pool.close().await;
    }

    /// Credited artists are protected until their recording is removed.
    #[tokio::test]
    async fn foreign_keys_restrict_artists_and_cascade_credits() {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let id = artist(&pool, "Existing").await;
        let recording =
            create(&pool, NewRecording::new("Recording", vec![id]).unwrap())
                .await
                .unwrap();
        assert!(
            sqlx::query("DELETE FROM artists WHERE id = ?")
                .bind(id)
                .execute(&pool)
                .await
                .is_err()
        );
        sqlx::query("DELETE FROM recordings WHERE id = ?")
            .bind(recording.id)
            .execute(&pool)
            .await
            .unwrap();
        let credits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recording_artists")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(credits, 0);
        pool.close().await;
    }
}
