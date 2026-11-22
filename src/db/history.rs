//! Atomic playback reports and account-scoped chronological history.

use crate::audio::history::{NewPlayback, PlaybackEvent};
use sqlx::{SqliteConnection, SqlitePool};

/// Expected history conflicts remain distinct from storage diagnostics.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HistoryError {
    /// The submitted recording is not in the shared catalog.
    #[error("Recording does not exist")]
    MissingRecording,
    /// An idempotency key already exists with different playback facts.
    #[error("event_id already identifies a different playback report")]
    Conflict,
    /// Permit five minutes of clock skew without accepting future history.
    #[error("started_at must not be more than five minutes in the future")]
    FutureTimestamp,
    /// Unexpected storage errors are reserved for server logs.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Accepted playback and whether this request inserted a new history item.
pub(crate) struct SavedPlayback {
    /// Newly inserted or previously accepted immutable playback facts.
    pub(crate) event: PlaybackEvent,
    /// False for an identical retry with the same account/event key.
    pub(crate) created: bool,
}

/// Insert or acknowledge a report; concurrent retries can never duplicate it.
pub(crate) async fn record(
    pool: &SqlitePool,
    user: i64,
    input: NewPlayback,
) -> Result<SavedPlayback, HistoryError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let existing: Option<PlaybackEvent> = sqlx::query_as(
        "SELECT h.id, h.event_id, h.recording_id, r.title, h.started_at, \
        h.listened_ms, h.duration_ms, h.kind, h.received_at \
        FROM playback_events h JOIN recordings r ON r.id = h.recording_id \
        WHERE h.user_id = ? AND h.event_id = ?",
    )
    .bind(user)
    .bind(&input.event_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(event) = existing {
        if !input.matches(&event) {
            return Err(HistoryError::Conflict);
        }
        tx.commit().await?;
        return Ok(SavedPlayback {
            event,
            created: false,
        });
    }
    let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(&mut *tx)
        .await?;
    if input.started_at > now.saturating_add(300) {
        return Err(HistoryError::FutureTimestamp);
    }
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO playback_events (user_id, event_id, recording_id, \
        started_at, listened_ms, duration_ms, kind) \
        SELECT ?, ?, ?, ?, ?, ?, ? WHERE EXISTS \
        (SELECT 1 FROM recordings WHERE id = ?) RETURNING id",
    )
    .bind(user)
    .bind(&input.event_id)
    .bind(input.recording_id)
    .bind(input.started_at)
    .bind(input.listened_ms)
    .bind(input.duration_ms)
    .bind(input.kind)
    .bind(input.recording_id)
    .fetch_optional(&mut *tx)
    .await?;
    let id = id.ok_or(HistoryError::MissingRecording)?;
    let event = read(&mut tx, user, id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    tx.commit().await?;
    Ok(SavedPlayback {
        event,
        created: true,
    })
}

/// Return an item only when it belongs to the authenticated account.
pub(crate) async fn find(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<Option<PlaybackEvent>, sqlx::Error> {
    read(&mut *pool.acquire().await?, user, id).await
}

/// Read an account-scoped item inside a pooled connection or transaction.
async fn read(
    connection: &mut SqliteConnection,
    user: i64,
    id: i64,
) -> Result<Option<PlaybackEvent>, sqlx::Error> {
    sqlx::query_as(
        "SELECT h.id, h.event_id, h.recording_id, r.title, h.started_at, \
        h.listened_ms, h.duration_ms, h.kind, h.received_at \
        FROM playback_events h JOIN recordings r ON r.id = h.recording_id \
        WHERE h.user_id = ? AND h.id = ?",
    )
    .bind(user)
    .bind(id)
    .fetch_optional(connection)
    .await
}

/// Read a bounded page by playback start, newest first; ID breaks time ties.
pub(crate) async fn list(
    pool: &SqlitePool,
    user: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<PlaybackEvent>, sqlx::Error> {
    sqlx::query_as(
        "SELECT h.id, h.event_id, h.recording_id, r.title, h.started_at, \
        h.listened_ms, h.duration_ms, h.kind, h.received_at \
        FROM playback_events h JOIN recordings r ON r.id = h.recording_id \
        WHERE h.user_id = ? ORDER BY h.started_at DESC, h.id DESC \
        LIMIT ? OFFSET ?",
    )
    .bind(user)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    //! Concurrent retries, immutable facts, isolation and chronological reads.

    use super::*;

    /// Seed accounts and recordings without creating physical audio files.
    async fn setup() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id, name) VALUES (1,'alice'),(2,'bob')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO recordings (id, title) VALUES (1,'Track'),(2,'Other')"
        ).execute(&pool).await.unwrap();
        (directory, pool)
    }

    /// A finished playback with a known duration and deterministic start time.
    fn input(key: &str, start: i64) -> NewPlayback {
        NewPlayback::new(key.into(), 1, start, 100, Some(200)).unwrap()
    }

    /// Retries cannot duplicate or rewrite a listen, even when concurrent.
    #[tokio::test]
    async fn serializes_retries_and_preserves_facts_across_restart() {
        let (directory, pool) = setup().await;
        let (first, second) = tokio::join!(
            record(&pool, 1, input("same", 10)),
            record(&pool, 1, input("same", 10)),
        );
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_ne!(first.created, second.created);
        assert_eq!(first.event.id, second.event.id);
        assert_eq!(first.event.received_at, second.event.received_at);
        for (recording, start, listened, duration) in [
            (2, 10, 100, Some(200)),
            (1, 11, 100, Some(200)),
            (1, 10, 101, Some(200)),
            (1, 10, 100, Some(201)),
            (1, 10, 100, None),
        ] {
            let changed = NewPlayback::new(
                "same".into(),
                recording,
                start,
                listened,
                duration,
            )
            .unwrap();
            assert!(matches!(
                record(&pool, 1, changed).await,
                Err(HistoryError::Conflict)
            ));
        }
        let bob = record(&pool, 2, input("same", 10)).await.unwrap();
        assert!(bob.created);
        assert_ne!(bob.event.id, first.event.id);
        assert!(find(&pool, 2, first.event.id).await.unwrap().is_none());
        assert!(find(&pool, 1, bob.event.id).await.unwrap().is_none());
        sqlx::query("UPDATE recordings SET title = 'Corrected' WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        let retry = record(&pool, 1, input("same", 10)).await.unwrap();
        assert!(!retry.created);
        assert_eq!(retry.event.title, "Corrected");
        assert_eq!(retry.event.received_at, first.event.received_at);
        pool.close().await;
        let pool = crate::db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let saved = find(&pool, 1, first.event.id).await.unwrap().unwrap();
        assert_eq!(saved.event_id, "same");
        assert_eq!(saved.received_at, first.event.received_at);
        assert_eq!(list(&pool, 1, 50, 0).await.unwrap().len(), 1);
        pool.close().await;
    }

    /// Reject future/missing targets and order offline reports by start.
    #[tokio::test]
    async fn orders_offline_reports_and_rejects_invalid_writes_atomically() {
        let (_directory, pool) = setup().await;
        let first = record(&pool, 1, input("first", 100)).await.unwrap();
        record(&pool, 1, input("older-uploaded-later", 1))
            .await
            .unwrap();
        let tie = record(&pool, 1, input("same-time", 100)).await.unwrap();
        assert_eq!(list(&pool, 1, 1, 0).await.unwrap()[0].id, tie.event.id);
        assert_eq!(list(&pool, 1, 1, 1).await.unwrap()[0].id, first.event.id);
        assert_eq!(list(&pool, 1, 1, 2).await.unwrap()[0].started_at, 1);
        assert!(list(&pool, 1, 50, 3).await.unwrap().is_empty());
        assert!(list(&pool, 2, 50, 0).await.unwrap().is_empty());
        assert!(matches!(
            record(&pool, 1, input("future", i64::MAX)).await,
            Err(HistoryError::FutureTimestamp)
        ));
        let missing =
            NewPlayback::new("missing".into(), 999, 0, 100, Some(200))
                .unwrap();
        assert!(matches!(
            record(&pool, 1, missing).await,
            Err(HistoryError::MissingRecording)
        ));
        assert_eq!(list(&pool, 1, 50, 0).await.unwrap().len(), 3);
        let reactions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recording_reactions")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reactions, 0);
        sqlx::query("DELETE FROM users WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert!(list(&pool, 1, 50, 0).await.unwrap().is_empty());
        let recordings: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(recordings, 2);
        pool.close().await;
    }
}
