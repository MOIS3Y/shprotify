//! Atomic personal queue snapshots with optimistic concurrency control.

use crate::audio::queue::{Queue, QueueTrack, validate_position};
use sqlx::{SqliteConnection, SqlitePool, types::Json};

/// Expected queue failures and private storage diagnostics.
#[derive(Debug, thiserror::Error)]
pub(crate) enum QueueError {
    /// Any stale update, including a retry after another client changed it.
    #[error("Queue version changed; reload before retrying")]
    StaleVersion,
    /// Submitted order contains an unknown recording.
    #[error("One or more recordings do not exist")]
    MissingRecording,
    /// Selection does not fit the effective order or offset is invalid.
    #[error("{0}")]
    InvalidPosition(&'static str),
    /// Unexpected diagnostics are reserved for server logs.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Read state and tracks from the same connection snapshot.
async fn read(
    connection: &mut SqliteConnection,
    user: i64,
) -> Result<Queue, sqlx::Error> {
    let state: Option<(i64, Option<i64>, i64)> = sqlx::query_as(
        "SELECT version, current_position, position_ms \
        FROM playback_queues WHERE user_id = ?",
    )
    .bind(user)
    .fetch_optional(&mut *connection)
    .await?;
    let (version, current_position, position_ms) =
        state.unwrap_or((0, None, 0));
    let items: Vec<QueueTrack> = sqlx::query_as(
        "SELECT q.position, q.recording_id, r.title \
        FROM playback_queue_tracks q JOIN recordings r \
        ON r.id = q.recording_id WHERE q.user_id = ? ORDER BY q.position",
    )
    .bind(user)
    .fetch_all(connection)
    .await?;
    Ok(Queue {
        version,
        current_position,
        position_ms,
        items,
    })
}

/// An untouched account has an empty queue at version zero without a write.
pub(crate) async fn get(
    pool: &SqlitePool,
    user: i64,
) -> Result<Queue, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let queue = read(&mut tx, user).await?;
    tx.commit().await?;
    Ok(queue)
}

/// Replace order and/or selection in one serialized transaction.
/// None for recordings preserves order, allowing efficient position updates.
/// No-ops preserve version; stale versions still conflict before comparison.
pub(crate) async fn update(
    pool: &SqlitePool,
    user: i64,
    version: i64,
    recordings: Option<&[i64]>,
    current: Option<i64>,
    position_ms: i64,
) -> Result<Queue, QueueError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let previous = read(&mut tx, user).await?;
    if previous.version != version {
        return Err(QueueError::StaleVersion);
    }
    let count = recordings.map_or(previous.items.len(), <[i64]>::len);
    validate_position(current, position_ms, count)
        .map_err(QueueError::InvalidPosition)?;
    let order_changed = recordings.is_some_and(|ids| {
        !previous
            .items
            .iter()
            .map(|item| item.recording_id)
            .eq(ids.iter().copied())
    });
    if !order_changed
        && previous.current_position == current
        && previous.position_ms == position_ms
    {
        tx.commit().await?;
        return Ok(previous);
    }
    if let Some(ids) = recordings.filter(|_| order_changed) {
        let missing: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM json_each(?) j \
            LEFT JOIN recordings r ON r.id = j.value WHERE r.id IS NULL)",
        )
        .bind(Json(ids))
        .fetch_one(&mut *tx)
        .await?;
        if missing {
            return Err(QueueError::MissingRecording);
        }
    }
    sqlx::query(
        "INSERT INTO playback_queues \
        (user_id, version, current_position, position_ms) VALUES (?,1,?,?) \
        ON CONFLICT(user_id) DO UPDATE SET version = version + 1, \
        current_position = excluded.current_position, \
        position_ms = excluded.position_ms",
    )
    .bind(user)
    .bind(current)
    .bind(position_ms)
    .execute(&mut *tx)
    .await?;
    if let Some(ids) = recordings.filter(|_| order_changed) {
        sqlx::query("DELETE FROM playback_queue_tracks WHERE user_id = ?")
            .bind(user)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO playback_queue_tracks \
            (user_id, position, recording_id) \
            SELECT ?, CAST(key AS INTEGER) + 1, value FROM json_each(?)",
        )
        .bind(user)
        .bind(Json(ids))
        .execute(&mut *tx)
        .await?;
    }
    let queue = read(&mut tx, user).await?;
    tx.commit().await?;
    Ok(queue)
}

#[cfg(test)]
mod tests {
    //! Version conflicts, occurrence semantics, rollback and persistence.

    use super::*;

    /// Seed two owners and two recordings without requiring audio files.
    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("queue.db"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id,name) VALUES (1,'alice'),(2,'bob')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO recordings (id,title) VALUES (1,'One'),(2,'Two')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (directory, pool)
    }

    /// Order, seeks and clearing share one version, including no-op checks.
    #[tokio::test]
    async fn persists_independent_queues_and_occurrence_selection() {
        let (directory, pool) = setup().await;
        let initial = get(&pool, 1).await.unwrap();
        assert_eq!(initial.version, 0);
        assert!(initial.items.is_empty());
        assert_eq!(initial.current_position, None);
        update(&pool, 1, 0, Some(&[]), None, 0).await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM playback_queues")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
        let created = update(&pool, 1, 0, Some(&[1, 2, 1]), Some(3), 123)
            .await
            .unwrap();
        assert_eq!(created.version, 1);
        assert_eq!(created.items[2].recording_id, 1);
        assert_eq!(created.current_position, Some(3));
        assert_eq!(get(&pool, 2).await.unwrap().version, 0);
        assert_eq!(
            update(&pool, 1, 1, Some(&[1, 2, 1]), Some(3), 123)
                .await
                .unwrap()
                .version,
            1
        );
        assert_eq!(
            update(&pool, 1, 1, None, Some(1), 55)
                .await
                .unwrap()
                .version,
            2
        );
        assert!(matches!(
            update(&pool, 1, 1, None, Some(1), 55).await,
            Err(QueueError::StaleVersion)
        ));
        let reordered = update(&pool, 1, 2, Some(&[2, 1, 1]), Some(2), 55)
            .await
            .unwrap();
        assert_eq!(reordered.version, 3);
        assert_eq!(reordered.items[0].recording_id, 2);
        sqlx::query("UPDATE recordings SET title = 'Corrected' WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(get(&pool, 1).await.unwrap().items[1].title, "Corrected");
        pool.close().await;
        let pool = crate::db::connect(&directory.path().join("queue.db"))
            .await
            .unwrap();
        let reopened = get(&pool, 1).await.unwrap();
        assert_eq!(reopened.version, 3);
        assert_eq!(reopened.current_position, Some(2));
        assert_eq!(reopened.position_ms, 55);
        let cleared = update(&pool, 1, 3, Some(&[]), None, 0).await.unwrap();
        assert_eq!(cleared.version, 4);
        assert!(cleared.items.is_empty());
        assert_eq!(
            update(&pool, 1, 4, None, None, 0).await.unwrap().version,
            4
        );
        let recordings: i64 =
            sqlx::query_scalar("SELECT count(*) FROM recordings")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(recordings, 2);
        let events: i64 =
            sqlx::query_scalar("SELECT count(*) FROM playback_events")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events, 0);
        pool.close().await;
    }

    /// Invalid references and cursor updates never partially mutate state.
    #[tokio::test]
    async fn rolls_back_failed_changes_and_accepts_large_orders() {
        let (_directory, pool) = setup().await;
        update(&pool, 1, 0, Some(&[1, 2]), Some(1), 10)
            .await
            .unwrap();
        assert!(matches!(
            update(&pool, 1, 1, Some(&[2, 999]), Some(2), 0).await,
            Err(QueueError::MissingRecording)
        ));
        for (ids, current, offset) in [
            (None, Some(3), 0),
            (Some(&[1][..]), Some(2), 0),
            (Some(&[][..]), Some(1), 0),
            (None, None, 1),
            (None, Some(1), -1),
        ] {
            assert!(matches!(
                update(&pool, 1, 1, ids, current, offset).await,
                Err(QueueError::InvalidPosition(_))
            ));
        }
        let state = get(&pool, 1).await.unwrap();
        assert_eq!(state.version, 1);
        assert_eq!(state.items[0].recording_id, 1);
        assert_eq!(state.position_ms, 10);
        // Simulate an unexpected storage failure after the original DELETE.
        sqlx::query(
            "CREATE TRIGGER reject_queue_insert BEFORE INSERT \
            ON playback_queue_tracks BEGIN SELECT RAISE(ABORT,'test'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            update(&pool, 1, 1, Some(&[2]), Some(1), 0).await,
            Err(QueueError::Storage(_))
        ));
        assert_eq!(get(&pool, 1).await.unwrap().version, 1);
        assert_eq!(get(&pool, 1).await.unwrap().items.len(), 2);
        sqlx::query("DROP TRIGGER reject_queue_insert")
            .execute(&pool)
            .await
            .unwrap();
        let ids = vec![1; crate::audio::queue::MAX_TRACKS];
        let large = update(&pool, 1, 1, Some(&ids), Some(10_000), 0)
            .await
            .unwrap();
        assert_eq!(large.items.len(), 10_000);
        assert_eq!(large.items[9999].position, 10_000);
        pool.close().await;
    }

    /// Two initial writers cannot both create version one or lose an update.
    #[tokio::test]
    async fn serializes_concurrent_order_and_playback_changes() {
        let (_directory, pool) = setup().await;
        let (a, b) = tokio::join!(
            update(&pool, 1, 0, Some(&[1]), Some(1), 0),
            update(&pool, 1, 0, Some(&[2]), Some(1), 0),
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(
            matches!(a, Err(QueueError::StaleVersion))
                || matches!(b, Err(QueueError::StaleVersion))
        );
        let (a, b) = tokio::join!(
            update(&pool, 1, 1, Some(&[]), None, 0),
            update(&pool, 1, 1, None, Some(1), 100),
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert!(
            matches!(a, Err(QueueError::StaleVersion))
                || matches!(b, Err(QueueError::StaleVersion))
        );
        assert_eq!(get(&pool, 1).await.unwrap().version, 2);
        pool.close().await;
    }
}
