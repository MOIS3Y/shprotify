//! Ranked catalog search maintained transactionally by SQLite FTS5 triggers.

use sqlx::SqlitePool;

use crate::library::search::{SearchHit, SearchKind};

/// Search a validated literal phrase, optionally restricting the entity kind.
///
/// Results rank by FTS5 BM25, then kind and numeric ID for stable ties. Bounds
/// apply to the combined result set; no per-hit queries or file reads occur.
pub(crate) async fn find(
    pool: &SqlitePool,
    phrase: &str,
    kind: Option<SearchKind>,
    limit: i64,
    offset: i64,
) -> Result<Vec<SearchHit>, sqlx::Error> {
    sqlx::query_as(
        "SELECT kind, CAST(entity_id AS INTEGER) AS id, text AS title \
         FROM catalog_search WHERE catalog_search MATCH ? \
         AND (? IS NULL OR kind = ?) \
         ORDER BY rank, kind, CAST(entity_id AS INTEGER) LIMIT ? OFFSET ?",
    )
    .bind(phrase)
    .bind(kind)
    .bind(kind)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    //! Unicode queries, literal syntax, paging, and atomic index changes.

    use super::*;
    use crate::{db, library::search::phrase};

    /// All categories share Unicode prefixes with stable combined pagination.
    #[tokio::test]
    async fn searches_unicode_categories_and_literal_phrases() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO artists (name) VALUES ('КИНО'), ('Кино')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (title) VALUES ('Кино ночью')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO releases (title, kind) \
            VALUES ('Кино ночью', 'album')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let query = phrase("ки").unwrap();
        let hits = find(&pool, &query, None, 50, 0).await.unwrap();
        assert_eq!(hits.len(), 4);
        assert_eq!(
            find(&pool, &query, Some(SearchKind::Artist), 50, 0)
                .await
                .unwrap()
                .len(),
            2
        );
        let page = find(&pool, &query, None, 1, 1).await.unwrap();
        assert_eq!(page[0].id, hits[1].id);
        assert_eq!(page[0].title, hits[1].title);
        assert!(find(&pool, &query, None, 50, 4).await.unwrap().is_empty());
        assert_eq!(
            find(&pool, &phrase("кино но").unwrap(), None, 50, 0)
                .await
                .unwrap()
                .len(),
            2
        );
        for query in [
            "но кино",
            "%",
            "_",
            "\"",
            "()",
            "*",
            "x OR кино",
            "' OR 1=1 --",
        ] {
            assert!(
                find(&pool, &phrase(query).unwrap(), None, 50, 0)
                    .await
                    .unwrap()
                    .is_empty(),
                "{query}"
            );
        }
        sqlx::query("INSERT INTO artists (name) VALUES ('Björk')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            find(&pool, &phrase("BJÖ").unwrap(), None, 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            find(&pool, &phrase("Bjo").unwrap(), None, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        pool.close().await;
    }

    /// Index changes commit or roll back with the catalog and survive restart.
    #[tokio::test]
    async fn follows_edits_deletes_and_transaction_rollbacks() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("catalog.db");
        let pool = db::connect(&database).await.unwrap();
        sqlx::query("INSERT INTO artists (name) VALUES ('Before')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE artists SET name = 'After'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            find(&pool, &phrase("Before").unwrap(), None, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("INSERT INTO recordings (title) VALUES ('Rolled back')")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert!(
            find(&pool, &phrase("Rolled").unwrap(), None, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        pool.close().await;
        let pool = db::connect(&database).await.unwrap();
        assert_eq!(
            find(&pool, &phrase("After").unwrap(), None, 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        sqlx::query("DELETE FROM artists")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            find(&pool, &phrase("After").unwrap(), None, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        pool.close().await;
    }
}
