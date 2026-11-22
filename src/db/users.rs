//! Local account management and revocable hashed bearer credentials.

use crate::auth::User;
use sqlx::SqlitePool;

/// Create an account and first credential atomically.
pub(crate) async fn create(
    pool: &SqlitePool,
    name: &str,
    digest: &str,
) -> Result<User, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let user: User = sqlx::query_as(
        "INSERT INTO users (name) VALUES (?) RETURNING id, name",
    )
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO user_keys (digest, user_id) VALUES (?, ?)")
        .bind(digest)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(user)
}

/// Issue an additional credential without replacing identity or preferences.
pub(crate) async fn issue(
    pool: &SqlitePool,
    name: &str,
    digest: &str,
) -> Result<User, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let user: User =
        sqlx::query_as("SELECT id, name FROM users WHERE name = ?")
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO user_keys (digest, user_id) VALUES (?, ?)")
        .bind(digest)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(user)
}

/// Revoke every credential for one account, retaining all personal data.
pub(crate) async fn revoke(
    pool: &SqlitePool,
    name: &str,
) -> Result<User, sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let user: User =
        sqlx::query_as("SELECT id, name FROM users WHERE name = ?")
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM user_keys WHERE user_id = ?")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(user)
}

/// Resolve a current credential digest; raw keys are never queried or stored.
pub(crate) async fn authenticate(
    pool: &SqlitePool,
    digest: &str,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as(
        "SELECT u.id, u.name FROM users u \
        JOIN user_keys k ON k.user_id = u.id WHERE k.digest = ?",
    )
    .bind(digest)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    //! Atomic key creation, uniqueness, and database credential hygiene.

    use super::*;

    /// Key failures leave no partial account; duplicate names stay unique.
    #[tokio::test]
    async fn stores_only_hashes_and_rolls_back_failed_account_creation() {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let token = crate::auth::token().unwrap();
        let hash = crate::auth::digest(&token);
        create(&pool, "alice", &hash).await.unwrap();
        assert!(
            create(&pool, "alice", &crate::auth::digest("other"))
                .await
                .is_err()
        );
        assert!(create(&pool, "bob", &hash).await.is_err());
        let accounts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(accounts, 1);
        let stored: String =
            sqlx::query_scalar("SELECT digest FROM user_keys")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, hash);
        assert_ne!(stored, token);
        assert!(revoke(&pool, "missing").await.is_err());
        assert!(authenticate(&pool, &hash).await.unwrap().is_some());
        pool.close().await;
    }
}
