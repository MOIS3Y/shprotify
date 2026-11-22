//! Owner-scoped playlists with atomic, version-checked metadata and track
//! edits.

use crate::playlists::{Playlist, PlaylistDetails, PlaylistTrack};
use sqlx::{SqliteConnection, SqlitePool, types::Json};

/// Expected playlist failures without exposing another owner's resources.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PlaylistError {
    /// Missing playlist and another user's playlist are indistinguishable.
    #[error("Playlist not found")]
    NotFound,
    /// The client edited an older snapshot; reload before retrying.
    #[error("Playlist version changed; reload before retrying")]
    StaleVersion,
    /// A submitted recording does not exist; all changes are rolled back.
    #[error("One or more recordings do not exist")]
    MissingRecording,
    /// Unexpected SQLite diagnostics are reserved for server logs.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// A page of ordered tracks paired with the exact version read in one
/// snapshot.
pub(crate) struct TrackSnapshot {
    /// Playlist version applicable to every returned position.
    pub(crate) version: i64,
    /// Ordered recording occurrences; titles are read from the catalog.
    pub(crate) items: Vec<PlaylistTrack>,
}

/// Create an empty private playlist; duplicate names are intentionally
/// allowed.
pub(crate) async fn create(
    pool: &SqlitePool,
    user: i64,
    details: PlaylistDetails,
) -> Result<Playlist, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO playlists (user_id, name, description) VALUES (?, ?, ?) \
        RETURNING id, name, description, version, created_at, updated_at",
    )
    .bind(user)
    .bind(details.name)
    .bind(details.description)
    .fetch_one(pool)
    .await
}

/// Read only playlists owned by the authenticated account, in ID order.
pub(crate) async fn list(
    pool: &SqlitePool,
    user: i64,
    limit: i64,
    offset: i64,
) -> Result<Vec<Playlist>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, description, version, created_at, updated_at \
        FROM playlists WHERE user_id = ? ORDER BY id LIMIT ? OFFSET ?",
    )
    .bind(user)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Find a playlist without disclosing whether another account owns its ID.
pub(crate) async fn find(
    pool: &SqlitePool,
    user: i64,
    id: i64,
) -> Result<Option<Playlist>, sqlx::Error> {
    read(&mut *pool.acquire().await?, user, id).await
}

/// Read metadata using an existing transaction or pooled connection.
async fn read(
    connection: &mut SqliteConnection,
    user: i64,
    id: i64,
) -> Result<Option<Playlist>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, description, version, created_at, updated_at \
        FROM playlists WHERE user_id = ? AND id = ?",
    )
    .bind(user)
    .bind(id)
    .fetch_optional(connection)
    .await
}

/// Check ownership before the expected version, retaining privacy on
/// conflicts.
async fn editable(
    connection: &mut SqliteConnection,
    user: i64,
    id: i64,
    version: i64,
) -> Result<Playlist, PlaylistError> {
    let playlist = read(connection, user, id)
        .await?
        .ok_or(PlaylistError::NotFound)?;
    if playlist.version != version {
        return Err(PlaylistError::StaleVersion);
    }
    Ok(playlist)
}

/// Apply metadata and advance a version within an already serialized mutation.
async fn update(
    connection: &mut SqliteConnection,
    user: i64,
    id: i64,
    name: &str,
    description: &str,
) -> Result<Playlist, sqlx::Error> {
    sqlx::query_as(
        "UPDATE playlists SET name = ?, description = ?, \
        version = version + 1, \
        updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
        WHERE user_id = ? AND id = ? \
        RETURNING id, name, description, version, created_at, updated_at",
    )
    .bind(name)
    .bind(description)
    .bind(user)
    .bind(id)
    .fetch_one(connection)
    .await
}

/// Replace metadata atomically; semantic no-ops retain timestamp and version.
pub(crate) async fn edit(
    pool: &SqlitePool,
    user: i64,
    id: i64,
    version: i64,
    details: PlaylistDetails,
) -> Result<Playlist, PlaylistError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let mut playlist = editable(&mut tx, user, id, version).await?;
    if playlist.name != details.name
        || playlist.description != details.description
    {
        playlist =
            update(&mut tx, user, id, &details.name, &details.description)
                .await?;
    }
    tx.commit().await?;
    Ok(playlist)
}

/// Delete an owned playlist and its tracks, protecting against stale clients.
pub(crate) async fn delete(
    pool: &SqlitePool,
    user: i64,
    id: i64,
    version: i64,
) -> Result<(), PlaylistError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    editable(&mut tx, user, id, version).await?;
    sqlx::query("DELETE FROM playlists WHERE user_id = ? AND id = ?")
        .bind(user)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Read one consistent content page/version without per-track SQL queries.
pub(crate) async fn tracks(
    pool: &SqlitePool,
    user: i64,
    id: i64,
    limit: i64,
    offset: i64,
) -> Result<TrackSnapshot, PlaylistError> {
    let mut tx = pool.begin().await?;
    let playlist = read(&mut tx, user, id)
        .await?
        .ok_or(PlaylistError::NotFound)?;
    let items = sqlx::query_as(
        "SELECT t.position, t.recording_id, r.title FROM playlist_tracks t \
        JOIN recordings r ON r.id = t.recording_id WHERE t.playlist_id = ? \
        ORDER BY t.position LIMIT ? OFFSET ?",
    )
    .bind(id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(TrackSnapshot {
        version: playlist.version,
        items,
    })
}

/// Replace all ordered occurrences atomically, rejecting stale or invalid
/// edits.
pub(crate) async fn replace_tracks(
    pool: &SqlitePool,
    user: i64,
    id: i64,
    version: i64,
    recordings: &[i64],
) -> Result<Playlist, PlaylistError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let playlist = editable(&mut tx, user, id, version).await?;
    let previous: Vec<i64> = sqlx::query_scalar(
        "SELECT recording_id FROM playlist_tracks \
        WHERE playlist_id = ? ORDER BY position",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    if previous == recordings {
        tx.commit().await?;
        return Ok(playlist);
    }
    let missing: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM json_each(?) j \
        LEFT JOIN recordings r ON r.id = j.value WHERE r.id IS NULL)",
    )
    .bind(Json(recordings))
    .fetch_one(&mut *tx)
    .await?;
    if missing {
        return Err(PlaylistError::MissingRecording);
    }
    sqlx::query("DELETE FROM playlist_tracks WHERE playlist_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO playlist_tracks (playlist_id, position, recording_id) \
        SELECT ?, CAST(key AS INTEGER) + 1, value FROM json_each(?)",
    )
    .bind(id)
    .bind(Json(recordings))
    .execute(&mut *tx)
    .await?;
    let playlist =
        update(&mut tx, user, id, &playlist.name, &playlist.description)
            .await?;
    tx.commit().await?;
    Ok(playlist)
}

#[cfg(test)]
mod tests {
    //! Atomic order replacement, stable no-ops and concurrent version
    //! conflicts.

    use super::*;

    /// Seed two owners and recordings without files or implicit favorites.
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
            "INSERT INTO recordings (id,title) VALUES (1,'One'),(2,'Two')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (directory, pool)
    }

    /// Canonical metadata shared by create and no-op update fixtures.
    fn details() -> PlaylistDetails {
        PlaylistDetails::new("Mix", "Description").unwrap()
    }

    /// Repeated recordings and reorders survive reopen; failures roll back
    /// fully.
    #[tokio::test]
    async fn replaces_order_atomically_and_preserves_catalog_and_noops() {
        let (directory, pool) = setup().await;
        let playlist = create(&pool, 1, details()).await.unwrap();
        let unchanged =
            edit(&pool, 1, playlist.id, 1, details()).await.unwrap();
        assert_eq!(unchanged.version, 1);
        assert_eq!(unchanged.updated_at, playlist.updated_at);
        let playlist = replace_tracks(&pool, 1, playlist.id, 1, &[1, 2, 1])
            .await
            .unwrap();
        assert_eq!(playlist.version, 2);
        let same = replace_tracks(&pool, 1, playlist.id, 2, &[1, 2, 1])
            .await
            .unwrap();
        assert_eq!(same.version, 2);
        assert_eq!(same.updated_at, playlist.updated_at);
        assert!(matches!(
            replace_tracks(&pool, 1, playlist.id, 2, &[1, 999]).await,
            Err(PlaylistError::MissingRecording)
        ));
        sqlx::query(
            "CREATE TRIGGER reject_track BEFORE INSERT ON playlist_tracks \
            WHEN new.recording_id = 2 BEGIN \
            SELECT RAISE(ABORT, 'fixture failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            replace_tracks(&pool, 1, playlist.id, 2, &[2]).await,
            Err(PlaylistError::Storage(_))
        ));
        sqlx::query("DROP TRIGGER reject_track")
            .execute(&pool)
            .await
            .unwrap();
        let page = tracks(&pool, 1, playlist.id, 2, 1).await.unwrap();
        assert_eq!(page.version, 2);
        assert_eq!(page.items[0].position, 2);
        assert_eq!(page.items[0].recording_id, 2);
        assert_eq!(page.items[1].position, 3);
        assert_eq!(page.items[1].recording_id, 1);
        let changed = replace_tracks(&pool, 1, playlist.id, 2, &[2, 1, 1])
            .await
            .unwrap();
        assert_eq!(changed.version, 3);
        let renamed = edit(
            &pool,
            1,
            playlist.id,
            3,
            PlaylistDetails::new("Renamed", "").unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(renamed.version, 4);
        assert_eq!(renamed.created_at, playlist.created_at);
        sqlx::query("UPDATE recordings SET title = 'Corrected' WHERE id = 2")
            .execute(&pool)
            .await
            .unwrap();
        let page = tracks(&pool, 1, playlist.id, 50, 0).await.unwrap();
        assert_eq!(page.items[0].title, "Corrected");
        assert_eq!(page.version, 4);
        pool.close().await;
        let pool = crate::db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        assert_eq!(
            find(&pool, 1, playlist.id).await.unwrap().unwrap().name,
            "Renamed"
        );
        assert_eq!(
            tracks(&pool, 1, playlist.id, 50, 0)
                .await
                .unwrap()
                .items
                .iter()
                .map(|t| t.recording_id)
                .collect::<Vec<_>>(),
            [2, 1, 1]
        );
        let cleared =
            replace_tracks(&pool, 1, playlist.id, 4, &[]).await.unwrap();
        assert_eq!(cleared.version, 5);
        assert!(
            tracks(&pool, 1, playlist.id, 50, 0)
                .await
                .unwrap()
                .items
                .is_empty()
        );
        delete(&pool, 1, playlist.id, 5).await.unwrap();
        assert!(find(&pool, 1, playlist.id).await.unwrap().is_none());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
        pool.close().await;
    }

    /// Two clients sharing a version cannot silently overwrite each other.
    #[tokio::test]
    async fn concurrent_edits_have_one_winner_and_stale_clients_cannot_delete()
    {
        let (_directory, pool) = setup().await;
        let playlist = create(&pool, 1, details()).await.unwrap();
        let (a, b) = tokio::join!(
            replace_tracks(&pool, 1, playlist.id, 1, &[1, 2]),
            replace_tracks(&pool, 1, playlist.id, 1, &[2, 1]),
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let error = if let Err(error) = a {
            error
        } else {
            b.unwrap_err()
        };
        assert!(matches!(error, PlaylistError::StaleVersion));
        assert!(matches!(
            delete(&pool, 1, playlist.id, 1).await,
            Err(PlaylistError::StaleVersion)
        ));
        let page = tracks(&pool, 1, playlist.id, 50, 0).await.unwrap();
        assert_eq!(page.version, 2);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].position, 1);
        assert_eq!(page.items[1].position, 2);
        delete(&pool, 1, playlist.id, 2).await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM playlist_tracks")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
        pool.close().await;
    }

    /// Ownership is checked before versions; user deletion cascades only
    /// lists.
    #[tokio::test]
    async fn scopes_every_operation_and_keeps_duplicate_names_independent() {
        let (_directory, pool) = setup().await;
        let first = create(&pool, 1, details()).await.unwrap();
        let second = create(&pool, 1, details()).await.unwrap();
        let bob = create(&pool, 2, details()).await.unwrap();
        assert_eq!(list(&pool, 1, 1, 1).await.unwrap()[0].id, second.id);
        assert_eq!(list(&pool, 2, 50, 0).await.unwrap().len(), 1);
        assert!(find(&pool, 2, first.id).await.unwrap().is_none());
        assert!(matches!(
            edit(&pool, 2, first.id, 999, details()).await,
            Err(PlaylistError::NotFound)
        ));
        assert!(matches!(
            replace_tracks(&pool, 2, first.id, 999, &[1]).await,
            Err(PlaylistError::NotFound)
        ));
        assert!(matches!(
            tracks(&pool, 2, first.id, 50, 0).await,
            Err(PlaylistError::NotFound)
        ));
        assert!(matches!(
            delete(&pool, 2, first.id, 999).await,
            Err(PlaylistError::NotFound)
        ));
        replace_tracks(&pool, 1, first.id, 1, &[1]).await.unwrap();
        assert!(
            sqlx::query("DELETE FROM recordings WHERE id = 1")
                .execute(&pool)
                .await
                .is_err()
        );
        sqlx::query("DELETE FROM users WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        assert!(list(&pool, 1, 50, 0).await.unwrap().is_empty());
        assert!(find(&pool, 2, bob.id).await.unwrap().is_some());
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM playlist_tracks")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
        pool.close().await;
    }
}
