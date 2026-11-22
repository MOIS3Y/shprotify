//! Scoped browser credentials remain dependent on their issuing API
//! credential.

use sqlx::SqlitePool;

/// Persist a scoped ticket only while its parent and file are still valid.
/// `None` covers revoked parents, unavailable files and the per-user bound.
/// All checks share the write transaction with insertion.
pub(crate) async fn issue(
    pool: &SqlitePool,
    parent: &str,
    user: i64,
    recording: i64,
    file: i64,
    digest: &str,
    ttl: u32,
) -> Result<Option<i64>, sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("DELETE FROM media_tickets WHERE expires_at <= unixepoch()")
        .execute(&mut *tx)
        .await?;
    let expires = sqlx::query_scalar(
        "INSERT INTO media_tickets \
        (digest,user_id,parent_digest,recording_id,file_id,expires_at) \
        SELECT ?1,?2,?3,?4,?5,min(unixepoch()+?6,parent.expires_at) \
        FROM (SELECT 9223372036854775807 expires_at FROM user_keys \
            WHERE digest=?3 AND user_id=?2 \
            UNION ALL SELECT expires_at FROM user_sessions \
            WHERE digest=?3 AND user_id=?2 AND expires_at>unixepoch()) parent \
        WHERE EXISTS(SELECT 1 FROM audio_files f \
            WHERE f.id=?5 AND f.recording_id=?4 AND NOT EXISTS( \
                SELECT 1 FROM file_removals d \
                WHERE d.file_id=f.id AND d.status='pending')) \
        AND (SELECT count(*) FROM media_tickets WHERE user_id=?2)<100 \
        RETURNING expires_at",
    )
    .bind(digest)
    .bind(user)
    .bind(parent)
    .bind(recording)
    .bind(file)
    .bind(ttl)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(expires)
}

/// Validate scope and current parent revocation/expiry on every range request.
pub(crate) async fn redeem(
    pool: &SqlitePool,
    digest: &str,
) -> Result<Option<(i64, i64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT t.recording_id,t.file_id FROM media_tickets t \
        JOIN audio_files f ON f.id=t.file_id \
            AND f.recording_id=t.recording_id \
        WHERE t.digest=? AND t.expires_at>unixepoch() \
        AND NOT EXISTS(SELECT 1 FROM file_removals d \
            WHERE d.file_id=f.id AND d.status='pending') \
        AND (EXISTS(SELECT 1 FROM user_keys k \
            WHERE k.digest=t.parent_digest AND k.user_id=t.user_id) \
        OR EXISTS(SELECT 1 FROM user_sessions s \
            WHERE s.digest=t.parent_digest AND s.user_id=t.user_id \
            AND s.expires_at>unixepoch()))",
    )
    .bind(digest)
    .fetch_optional(pool)
    .await
}
