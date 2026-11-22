//! Atomic personal preferences and favorites without duplicate playlist state.

use crate::library::reactions::{Favorite, Reaction, ReactionState};
use sqlx::{SqliteConnection, SqlitePool};

/// Return a neutral/current reaction, or None for a missing recording.
pub(crate) async fn find(
    pool: &SqlitePool,
    user: i64,
    recording: i64,
) -> Result<Option<ReactionState>, sqlx::Error> {
    read(&mut *pool.acquire().await?, user, recording).await
}

/// Read one user-scoped reaction inside a connection or transaction.
async fn read(
    connection: &mut SqliteConnection,
    user: i64,
    recording: i64,
) -> Result<Option<ReactionState>, sqlx::Error> {
    sqlx::query_as(
        "SELECT r.id AS recording_id, p.reaction, p.changed_at \
        FROM recordings r LEFT JOIN recording_reactions p \
        ON p.recording_id = r.id AND p.user_id = ? WHERE r.id = ?",
    )
    .bind(user)
    .bind(recording)
    .fetch_optional(connection)
    .await
}

/// Change/clear a preference atomically; repeated reactions retain their time.
pub(crate) async fn set(
    pool: &SqlitePool,
    user: i64,
    recording: i64,
    reaction: Option<Reaction>,
) -> Result<Option<ReactionState>, sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    if read(&mut tx, user, recording).await?.is_none() {
        return Ok(None);
    }
    if let Some(reaction) = reaction {
        sqlx::query(
            "INSERT INTO recording_reactions \
            (user_id, recording_id, reaction, changed_at) \
            VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
            ON CONFLICT(user_id, recording_id) DO UPDATE \
            SET reaction = excluded.reaction, \
            changed_at = excluded.changed_at \
            WHERE recording_reactions.reaction != excluded.reaction",
        )
        .bind(user)
        .bind(recording)
        .bind(reaction)
        .execute(&mut *tx)
        .await?;
    } else {
        sqlx::query(
            "DELETE FROM recording_reactions \
            WHERE user_id = ? AND recording_id = ?",
        )
        .bind(user)
        .bind(recording)
        .execute(&mut *tx)
        .await?;
    }
    // Explicit reaffirmations and clears matter even when the visible reaction
    // stays unchanged: a delayed external import must not restore an old like.
    sqlx::query(
        "INSERT INTO recording_reaction_decisions \
        (user_id,recording_id,changed_at) \
        VALUES (?,?,strftime('%Y-%m-%dT%H:%M:%fZ','now')) \
        ON CONFLICT(user_id,recording_id) DO UPDATE \
        SET changed_at=excluded.changed_at",
    )
    .bind(user)
    .bind(recording)
    .execute(&mut *tx)
    .await?;
    let result = read(&mut tx, user, recording).await?;
    tx.commit().await?;
    Ok(result)
}

/// List current likes newest first; recording ID descending breaks time ties.
pub(crate) async fn favorites(
    pool: &SqlitePool,
    user: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Favorite>, sqlx::Error> {
    sqlx::query_as(
        "SELECT r.id AS recording_id, r.title, \
        p.changed_at AS added_at FROM recording_reactions p \
        JOIN recordings r ON r.id = p.recording_id \
        WHERE p.user_id = ? AND p.reaction = 'like' \
        ORDER BY p.changed_at DESC, p.recording_id DESC LIMIT ? OFFSET ?",
    )
    .bind(user)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    //! Chronology, idempotence, and concurrent preference changes.

    use super::*;

    /// Repeated likes retain order; concurrent updates keep one row.
    #[tokio::test]
    async fn preserves_chronology_and_serializes_concurrent_changes() {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO users (id, name) VALUES (1, 'alice')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO recordings (id, title) VALUES (1,'One'),(2,'Two')",
        )
        .execute(&pool)
        .await
        .unwrap();
        set(&pool, 1, 1, Some(Reaction::Like)).await.unwrap();
        set(&pool, 1, 2, Some(Reaction::Like)).await.unwrap();
        sqlx::query(
            "UPDATE recording_reactions SET changed_at = \
            '2000-01-01T00:00:00.000Z' WHERE recording_id = 2",
        )
        .execute(&pool)
        .await
        .unwrap();
        set(&pool, 1, 2, Some(Reaction::Like)).await.unwrap();
        assert_eq!(
            favorites(&pool, 1, 1, 0).await.unwrap()[0].recording_id,
            1
        );
        assert_eq!(
            favorites(&pool, 1, 1, 1).await.unwrap()[0].recording_id,
            2
        );
        set(&pool, 1, 2, None).await.unwrap();
        set(&pool, 1, 2, Some(Reaction::Like)).await.unwrap();
        assert!(
            find(&pool, 1, 2)
                .await
                .unwrap()
                .unwrap()
                .changed_at
                .unwrap()
                .starts_with("20")
        );
        let (a, b) = tokio::join!(
            set(&pool, 1, 1, Some(Reaction::Like)),
            set(&pool, 1, 1, Some(Reaction::Dislike)),
        );
        a.unwrap();
        b.unwrap();
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) \
            FROM recording_reactions WHERE user_id = 1 AND recording_id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);
        let liked = matches!(
            find(&pool, 1, 1).await.unwrap().unwrap().reaction,
            Some(Reaction::Like)
        );
        assert_eq!(
            favorites(&pool, 1, 50, 0).await.unwrap().len(),
            1 + usize::from(liked)
        );
        pool.close().await;
    }
}
