//! SQLite persistence for the shared artist catalog.

use sqlx::SqlitePool;

use crate::library::{Artist, ArtistName};

/// Insert a validated artist and return its assigned identifier and name.
pub(crate) async fn create(
    pool: &SqlitePool,
    name: &ArtistName,
) -> Result<Artist, sqlx::Error> {
    sqlx::query_as("INSERT INTO artists (name) VALUES (?) RETURNING id, name")
        .bind(name.as_str())
        .fetch_one(pool)
        .await
}

/// Find an artist by identifier; missing identifiers return `None`.
pub(crate) async fn find(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<Artist>, sqlx::Error> {
    sqlx::query_as("SELECT id, name FROM artists WHERE id = ?")
        .bind(id)
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
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Artist>, sqlx::Error> {
        sqlx::query_as(
            "SELECT id, name FROM artists ORDER BY id LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    }
}

#[cfg(test)]
mod tests {
    //! Migration-backed persistence, duplicate names, and deterministic pages.

    use super::fixtures::list;
    use super::*;

    /// Distinct artists may have identical names and survive reopening.
    #[tokio::test]
    async fn persists_distinct_artists_with_equal_names() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.db");
        let pool = crate::db::connect(&path).await.unwrap();
        let name = ArtistName::parse("Кино").unwrap();
        let first = create(&pool, &name).await.unwrap();
        let second = create(&pool, &name).await.unwrap();
        assert_ne!(first.id, second.id);
        pool.close().await;

        let pool = crate::db::connect(&path).await.unwrap();
        assert_eq!(find(&pool, first.id).await.unwrap().unwrap().name, "Кино");
        let page = list(&pool, 1, 1).await.unwrap();
        assert_eq!(page[0].id, second.id);
        assert!(find(&pool, second.id + 1).await.unwrap().is_none());
        assert!(list(&pool, 50, 2).await.unwrap().is_empty());
        pool.close().await;
    }
}
