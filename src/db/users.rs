//! Local account management and revocable hashed bearer credentials.

use crate::auth::{Role, User};
use sqlx::SqlitePool;

/// Create an account and first credential atomically.
pub(crate) async fn create(
    pool: &SqlitePool,
    name: &str,
    digest: &str,
) -> Result<User, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let user: User = sqlx::query_as(
        "INSERT INTO users (name) VALUES (?) RETURNING id, name, role",
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
        sqlx::query_as("SELECT id, name, role FROM users WHERE name = ?")
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

/// Revoke all personal keys, retaining password sessions and personal data.
pub(crate) async fn revoke(
    pool: &SqlitePool,
    name: &str,
) -> Result<User, sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let user: User =
        sqlx::query_as("SELECT id, name, role FROM users WHERE name = ?")
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

/// Resolve a key or unexpired session digest using current account rights.
pub(crate) async fn authenticate(
    pool: &SqlitePool,
    digest: &str,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as(
        "SELECT u.id, u.name, u.role FROM users u \
        WHERE u.id IN (SELECT user_id FROM user_keys WHERE digest = ? \
        UNION ALL SELECT user_id FROM user_sessions \
        WHERE digest = ? AND expires_at > unixepoch())",
    )
    .bind(digest)
    .bind(digest)
    .fetch_optional(pool)
    .await
}

/// Change permissions without replacing account, keys, or personal data.
pub(crate) async fn set_role(
    pool: &SqlitePool,
    name: &str,
    role: crate::auth::Role,
) -> Result<User, sqlx::Error> {
    sqlx::query_as(
        "UPDATE users SET role = ? WHERE name = ? \
        RETURNING id, name, role",
    )
    .bind(role)
    .bind(name)
    .fetch_one(pool)
    .await
}

/// Create a password account without implicitly issuing a permanent key.
pub(crate) async fn create_password(
    pool: &SqlitePool,
    name: &str,
    role: Role,
    hash: &str,
) -> Result<User, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let user: User = sqlx::query_as(
        "INSERT INTO users (name, role) VALUES (?, ?) \
        RETURNING id, name, role",
    )
    .bind(name)
    .bind(role)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO user_passwords (user_id, hash) VALUES (?, ?)")
        .bind(user.id)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(user)
}

/// Return public identities in stable identifier order, without credentials.
pub(crate) async fn list(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
) -> Result<Vec<User>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, name, role FROM users ORDER BY id LIMIT ? OFFSET ?",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

/// Find an account by stable identifier, without password or token material.
pub(crate) async fn find(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as("SELECT id, name, role FROM users WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Set the current role; local operator access can recover the last admin.
pub(crate) async fn set_role_by_id(
    pool: &SqlitePool,
    id: i64,
    role: Role,
) -> Result<User, sqlx::Error> {
    sqlx::query_as(
        "UPDATE users SET role = ? WHERE id = ? RETURNING id, name, role",
    )
    .bind(role)
    .bind(id)
    .fetch_one(pool)
    .await
}

/// Issue a personal key atomically for an existing stable account identity.
pub(crate) async fn issue_by_id(
    pool: &SqlitePool,
    id: i64,
    digest: &str,
) -> Result<User, sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let user: User =
        sqlx::query_as("SELECT id, name, role FROM users WHERE id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO user_keys (digest, user_id) VALUES (?, ?)")
        .bind(digest)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(user)
}

/// Revoke all personal keys for an existing account, retaining its sessions.
pub(crate) async fn revoke_by_id(
    pool: &SqlitePool,
    id: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let _: i64 = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_keys WHERE user_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Atomic key creation, uniqueness, and database credential hygiene.

    use super::*;

    /// A failed password insert rolls back account identity and permissions.
    #[tokio::test]
    async fn password_account_creation_is_atomic_and_does_not_issue_keys() {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_password BEFORE INSERT ON user_passwords \
            WHEN new.hash = 'reject' BEGIN \
            SELECT RAISE(ABORT, 'fixture failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            create_password(&pool, "alice", Role::Admin, "reject")
                .await
                .is_err()
        );
        assert!(list(&pool, 50, 0).await.unwrap().is_empty());
        let alice = create_password(&pool, "alice", Role::Member, "hash")
            .await
            .unwrap();
        assert_eq!(
            find(&pool, alice.id).await.unwrap().unwrap().role,
            Role::Member
        );
        let keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(keys, 0);
        assert!(
            create_password(&pool, "alice", Role::Admin, "other")
                .await
                .is_err()
        );
        assert_eq!(list(&pool, 50, 0).await.unwrap().len(), 1);
        let hash: String = sqlx::query_scalar(
            "SELECT hash FROM user_passwords WHERE user_id = ?",
        )
        .bind(alice.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(hash, "hash");
        assert!(list(&pool, 1, 1).await.unwrap().is_empty());
        pool.close().await;
    }

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
