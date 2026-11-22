//! Password credentials and independently revocable, expiring sessions.

use crate::auth::User;
use sqlx::SqlitePool;

/// Credential snapshot used to reject login racing with a password reset.
#[derive(sqlx::FromRow)]
pub(crate) struct Credential {
    /// Account owning the credential.
    pub(crate) user_id: i64,
    /// Salted Argon2id PHC string, never exposed to clients or logs.
    pub(crate) hash: String,
}

/// Read a password snapshot; key-only accounts have no password row.
pub(crate) async fn credential(
    pool: &SqlitePool,
    name: &str,
) -> Result<Option<Credential>, sqlx::Error> {
    sqlx::query_as(
        "SELECT p.user_id, p.hash FROM user_passwords p \
        JOIN users u ON u.id = p.user_id WHERE u.name = ?",
    )
    .bind(name)
    .fetch_optional(pool)
    .await
}

/// Set a password and revoke all sessions atomically, retaining personal keys.
pub(crate) async fn set_password(
    pool: &SqlitePool,
    name: &str,
    hash: &str,
) -> Result<User, sqlx::Error> {
    let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = ?")
        .bind(name)
        .fetch_one(pool)
        .await?;
    set_password_by_id(pool, id, hash).await
}

/// Reset an existing account password and revoke its sessions atomically.
pub(crate) async fn set_password_by_id(
    pool: &SqlitePool,
    id: i64,
    hash: &str,
) -> Result<User, sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let user: User =
        sqlx::query_as("SELECT id, name, role FROM users WHERE id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query(
        "INSERT INTO user_passwords (user_id, hash) VALUES (?, ?) \
        ON CONFLICT(user_id) DO UPDATE SET hash = excluded.hash",
    )
    .bind(user.id)
    .bind(hash)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM user_sessions WHERE user_id = ?")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(user)
}

/// Persist a digest only if the verified password snapshot is still current.
/// Add the configured seconds to the database clock and return Unix expiry,
/// or None if the password changed during hashing. Existing rows keep expiry.
pub(crate) async fn create(
    pool: &SqlitePool,
    credential: &Credential,
    digest: &str,
    lifetime_seconds: u32,
) -> Result<Option<i64>, sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("DELETE FROM user_sessions WHERE expires_at <= unixepoch()")
        .execute(&mut *tx)
        .await?;
    let expiry = sqlx::query_scalar(
        "INSERT INTO user_sessions (digest, user_id, expires_at) \
        SELECT ?, user_id, unixepoch() + ? FROM user_passwords \
        WHERE user_id = ? AND hash = ? RETURNING expires_at",
    )
    .bind(digest)
    .bind(i64::from(lifetime_seconds))
    .bind(credential.user_id)
    .bind(&credential.hash)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(expiry)
}

/// Revoke exactly the authenticated session, leaving other sessions and keys.
pub(crate) async fn revoke(
    pool: &SqlitePool,
    digest: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM user_sessions WHERE digest = ?")
        .bind(digest)
        .execute(pool)
        .await?;
    Ok(())
}

/// Revoke all sessions for an existing account, retaining password and keys.
pub(crate) async fn revoke_all(
    pool: &SqlitePool,
    id: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let _: i64 = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM user_sessions WHERE user_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Durable sessions, password reset races and independent personal keys.

    use super::*;
    use crate::{auth, db};

    /// Expiry/reset/logout survive reopen and do not invalidate personal keys.
    #[tokio::test]
    async fn session_lifecycle_and_password_snapshot_are_atomic() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.db");
        let pool = db::connect(&path).await.unwrap();
        let key = auth::digest(&auth::token().unwrap());
        let user = db::users::create(&pool, "alice", &key).await.unwrap();
        assert!(credential(&pool, "alice").await.unwrap().is_none());
        assert!(set_password(&pool, "missing", "hash").await.is_err());
        set_password(&pool, "alice", "first-hash").await.unwrap();
        let snapshot = credential(&pool, "alice").await.unwrap().unwrap();
        let first = auth::digest(&auth::session_token().unwrap());
        let second = auth::digest(&auth::session_token().unwrap());
        let first_expiry = create(&pool, &snapshot, &first, 3600)
            .await
            .unwrap()
            .unwrap();
        let second_expiry = create(&pool, &snapshot, &second, 60)
            .await
            .unwrap()
            .unwrap();
        assert!((3530..=3550).contains(&(first_expiry - second_expiry)));
        pool.close().await;
        let pool = db::connect(&path).await.unwrap();
        let stored_expiry: i64 = sqlx::query_scalar(
            "SELECT expires_at FROM user_sessions WHERE digest = ?",
        )
        .bind(&first)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored_expiry, first_expiry);
        assert_eq!(
            db::users::authenticate(&pool, &first)
                .await
                .unwrap()
                .unwrap()
                .id,
            user.id
        );
        revoke(&pool, &first).await.unwrap();
        assert!(
            db::users::authenticate(&pool, &first)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db::users::authenticate(&pool, &second)
                .await
                .unwrap()
                .is_some()
        );
        sqlx::query("UPDATE user_sessions SET expires_at = unixepoch()")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            db::users::authenticate(&pool, &second)
                .await
                .unwrap()
                .is_none()
        );
        create(&pool, &snapshot, &first, 3600)
            .await
            .unwrap()
            .unwrap();
        let expired: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_sessions WHERE digest = ?",
        )
        .bind(&second)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(expired, 0);
        set_password(&pool, "alice", "second-hash").await.unwrap();
        assert!(
            db::users::authenticate(&pool, &first)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            create(&pool, &snapshot, &second, 3600)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db::users::authenticate(&pool, &key)
                .await
                .unwrap()
                .is_some()
        );
        let current = credential(&pool, "alice").await.unwrap().unwrap();
        create(&pool, &current, &second, 3600)
            .await
            .unwrap()
            .unwrap();
        db::users::revoke(&pool, "alice").await.unwrap();
        assert!(
            db::users::authenticate(&pool, &key)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db::users::authenticate(&pool, &second)
                .await
                .unwrap()
                .is_some()
        );
        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(user.id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            db::users::authenticate(&pool, &second)
                .await
                .unwrap()
                .is_none()
        );
        assert!(credential(&pool, "alice").await.unwrap().is_none());
        pool.close().await;
    }
}
