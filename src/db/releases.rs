//! Atomic release creation and ordered catalog reads in SQLite.

use sqlx::SqlitePool;

use crate::library::releases::{NewRelease, Release, ReleaseKind};

/// Creation failures distinguish unknown credited artists from storage errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CreateReleaseError {
    /// A credited artist does not exist; the transaction is rolled back.
    #[error("A credited artist does not exist")]
    UnknownArtist,
    /// Unexpected SQL failure, reserved for server-side diagnostics.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Insert the release and ordered credits in one transaction.
///
/// SQLx rolls back an uncommitted transaction on early return, including when
/// a later credit fails after earlier credits were inserted successfully.
pub(crate) async fn create(
    pool: &SqlitePool,
    input: NewRelease,
) -> Result<Release, CreateReleaseError> {
    let mut transaction = pool.begin().await?;
    let id = sqlx::query_scalar(
        "INSERT INTO releases (title, kind) VALUES (?, ?) RETURNING id",
    )
    .bind(&input.title)
    .bind(input.kind)
    .fetch_one(&mut *transaction)
    .await?;
    for (artist_id, position) in input.artist_ids.iter().zip(0_i64..) {
        let result = sqlx::query(
            "INSERT INTO release_artists (release_id, artist_id, position) \
             VALUES (?, ?, ?)",
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
                return Err(CreateReleaseError::UnknownArtist);
            }
            return Err(error.into());
        }
    }
    transaction.commit().await?;
    Ok(Release {
        id,
        title: input.title,
        kind: input.kind,
        artist_ids: input.artist_ids,
    })
}

/// One row per joined credit; uncredited releases have a null artist ID.
#[derive(sqlx::FromRow)]
struct ReleaseRow {
    /// Release identifier used to group consecutive joined rows.
    id: i64,
    /// Stored display title.
    title: String,
    /// Stored primary classification.
    kind: ReleaseKind,
    /// One credited artist, if present.
    artist_id: Option<i64>,
}

/// Reassemble ordered join rows without issuing one query per release.
fn collect_releases(rows: Vec<ReleaseRow>) -> Vec<Release> {
    let mut releases: Vec<Release> = Vec::new();
    for row in rows {
        if let Some(last) = releases.last_mut().filter(|r| r.id == row.id) {
            last.artist_ids.extend(row.artist_id);
            continue;
        }
        releases.push(Release {
            id: row.id,
            title: row.title,
            kind: row.kind,
            artist_ids: row.artist_id.into_iter().collect(),
        });
    }
    releases
}

/// Fetch one release and its ordered credits in a single consistent query.
pub(crate) async fn find(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<Release>, sqlx::Error> {
    let rows = sqlx::query_as(
        "SELECT r.id, r.title, r.kind, a.artist_id FROM releases r \
         LEFT JOIN release_artists a ON a.release_id = r.id \
         WHERE r.id = ? ORDER BY a.position",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(collect_releases(rows).into_iter().next())
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
    ) -> Result<Vec<Release>, sqlx::Error> {
        let rows = sqlx::query_as(
            "WITH page AS (SELECT id, title, kind FROM releases \
             ORDER BY id LIMIT ? OFFSET ?) \
         SELECT p.id, p.title, p.kind, a.artist_id FROM page p \
         LEFT JOIN release_artists a ON a.release_id = p.id \
         ORDER BY p.id, a.position",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;
        Ok(collect_releases(rows))
    }
}

#[cfg(test)]
mod tests {
    //! Atomic credits, database constraints, and whole-release pagination.

    use crate::library::ArtistName;

    use super::fixtures::list;
    use super::*;

    /// Create a catalog artist for a foreign-key-backed release credit.
    async fn artist(pool: &SqlitePool, name: &str) -> i64 {
        crate::db::artists::create(pool, &ArtistName::parse(name).unwrap())
            .await
            .unwrap()
            .id
    }

    /// Joined pages preserve all credits, their order, and duplicate titles.
    #[tokio::test]
    async fn persists_ordered_credits_and_pages_whole_releases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.db");
        let pool = crate::db::connect(&path).await.unwrap();
        let first = artist(&pool, "First").await;
        let second = artist(&pool, "Second").await;
        let release = create(
            &pool,
            NewRelease::new("Shared", ReleaseKind::Album, vec![second, first])
                .unwrap(),
        )
        .await
        .unwrap();
        let compilation = create(
            &pool,
            NewRelease::new("Shared", ReleaseKind::Compilation, vec![])
                .unwrap(),
        )
        .await
        .unwrap();
        pool.close().await;

        let pool = crate::db::connect(&path).await.unwrap();
        let fetched = find(&pool, release.id).await.unwrap().unwrap();
        assert_eq!(fetched.artist_ids, [second, first]);
        assert!(matches!(fetched.kind, ReleaseKind::Album));
        let page = list(&pool, 1, 0).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].artist_ids, [second, first]);
        let page = list(&pool, 1, 1).await.unwrap();
        assert_eq!(page[0].id, compilation.id);
        assert!(page[0].artist_ids.is_empty());
        assert!(list(&pool, 50, 2).await.unwrap().is_empty());
        assert!(find(&pool, compilation.id + 1).await.unwrap().is_none());
        pool.close().await;
    }

    /// A failing later credit rolls back the release and earlier credits.
    #[tokio::test]
    async fn unknown_artist_rolls_back_the_entire_creation() {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let id = artist(&pool, "Existing").await;
        let error = create(
            &pool,
            NewRelease::new("Failed", ReleaseKind::Single, vec![id, id + 1])
                .unwrap(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, CreateReleaseError::UnknownArtist));
        let releases: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM releases")
                .fetch_one(&pool)
                .await
                .unwrap();
        let credits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM release_artists")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((releases, credits), (0, 0));
        let valid = create(
            &pool,
            NewRelease::new("Valid", ReleaseKind::Ep, vec![id]).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(valid.artist_ids, [id]);
        pool.close().await;
    }

    /// Credits restrict artist deletion and cascade when a release is deleted.
    #[tokio::test]
    async fn foreign_keys_preserve_catalog_integrity() {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let id = artist(&pool, "Existing").await;
        let release = create(
            &pool,
            NewRelease::new("Release", ReleaseKind::Other, vec![id]).unwrap(),
        )
        .await
        .unwrap();
        assert!(
            sqlx::query("DELETE FROM artists WHERE id = ?")
                .bind(id)
                .execute(&pool)
                .await
                .is_err()
        );
        sqlx::query("DELETE FROM releases WHERE id = ?")
            .bind(release.id)
            .execute(&pool)
            .await
            .unwrap();
        let credits: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM release_artists")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(credits, 0);
        pool.close().await;
    }
}
