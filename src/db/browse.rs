//! Snapshot-consistent rich catalog pages using static parameterized SQL.

use crate::library::{
    browse::{
        ArtistQuery, ArtistSummary, LibraryStats, RecordingQuery,
        RecordingSummary, ReleaseQuery, ReleaseSummary,
    },
    release_tracks::ReleaseTrack,
};
use sqlx::{SqlitePool, types::Json};

/// Recordings are filtered before paging; credits/files stay complete.
pub(crate) async fn recordings(
    pool: &SqlitePool,
    q: &RecordingQuery,
) -> Result<(Vec<RecordingSummary>, i64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let total = sqlx::query_scalar(
        "SELECT count(*) FROM client_recordings r \
        WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM recording_artists a \
            WHERE a.recording_id=r.id AND a.artist_id=?1)) \
        AND (?2 IS NULL OR EXISTS(SELECT 1 FROM release_tracks t \
            WHERE t.recording_id=r.id AND t.release_id=?2)) \
        AND (?3 IS NULL OR r.has_audio=?3)",
    )
    .bind(q.artist_id)
    .bind(q.release_id)
    .bind(q.has_audio)
    .fetch_one(&mut *tx)
    .await?;
    let items = sqlx::query_as(
        "SELECT * FROM client_recordings r \
        WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM recording_artists a \
            WHERE a.recording_id=r.id AND a.artist_id=?1)) \
        AND (?2 IS NULL OR EXISTS(SELECT 1 FROM release_tracks t \
            WHERE t.recording_id=r.id AND t.release_id=?2)) \
        AND (?3 IS NULL OR r.has_audio=?3) ORDER BY \
        CASE WHEN ?4='duration' THEN duration_ms IS NULL ELSE 0 END, \
        CASE WHEN ?4='title' AND ?5=0 THEN title END COLLATE NOCASE ASC, \
        CASE WHEN ?4='title' AND ?5=1 THEN title END COLLATE NOCASE DESC, \
        CASE WHEN ?4='duration' AND ?5=0 THEN duration_ms END ASC, \
        CASE WHEN ?4='duration' AND ?5=1 THEN duration_ms END DESC, \
        CASE WHEN ?5=0 THEN id END ASC, CASE WHEN ?5=1 THEN id END DESC \
        LIMIT ?6 OFFSET ?7",
    )
    .bind(q.artist_id)
    .bind(q.release_id)
    .bind(q.has_audio)
    .bind(q.sort.as_str())
    .bind(q.order.descending())
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((items, total))
}

/// Resolve a bounded set of IDs in caller order with one catalog query.
/// The API returns missing identities explicitly rather than inventing rows.
pub(crate) async fn recording_batch(
    pool: &SqlitePool,
    ids: &[i64],
) -> Result<Vec<RecordingSummary>, sqlx::Error> {
    sqlx::query_as(
        "SELECT r.* FROM json_each(?) wanted \
        JOIN client_recordings r ON r.id=wanted.value \
        ORDER BY CAST(wanted.key AS INTEGER)",
    )
    .bind(Json(ids))
    .fetch_all(pool)
    .await
}

/// One recording projection, without a separate lookup per file or artist.
pub(crate) async fn recording(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<RecordingSummary>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM client_recordings WHERE id=?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Discography/appearance filters operate on whole releases, not credit rows.
pub(crate) async fn releases(
    pool: &SqlitePool,
    q: &ReleaseQuery,
) -> Result<(Vec<ReleaseSummary>, i64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let total = sqlx::query_scalar(
        "SELECT count(*) FROM client_releases r \
        WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM release_artists a \
            WHERE a.release_id=r.id AND a.artist_id=?1)) \
        AND (?2 IS NULL OR EXISTS(SELECT 1 FROM release_tracks t \
            WHERE t.release_id=r.id AND t.recording_id=?2)) \
        AND (?3 IS NULL OR r.kind=?3) AND (?4 IS NULL OR r.has_audio=?4)",
    )
    .bind(q.artist_id)
    .bind(q.recording_id)
    .bind(q.kind)
    .bind(q.has_audio)
    .fetch_one(&mut *tx)
    .await?;
    let items = sqlx::query_as(
        "SELECT * FROM client_releases r \
        WHERE (?1 IS NULL OR EXISTS(SELECT 1 FROM release_artists a \
            WHERE a.release_id=r.id AND a.artist_id=?1)) \
        AND (?2 IS NULL OR EXISTS(SELECT 1 FROM release_tracks t \
            WHERE t.release_id=r.id AND t.recording_id=?2)) \
        AND (?3 IS NULL OR r.kind=?3) AND (?4 IS NULL OR r.has_audio=?4) \
        ORDER BY CASE WHEN ?5='duration' THEN duration_ms IS NULL ELSE 0 END, \
        CASE WHEN ?5='title' AND ?6=0 THEN title END COLLATE NOCASE ASC, \
        CASE WHEN ?5='title' AND ?6=1 THEN title END COLLATE NOCASE DESC, \
        CASE WHEN ?5='duration' AND ?6=0 THEN duration_ms END ASC, \
        CASE WHEN ?5='duration' AND ?6=1 THEN duration_ms END DESC, \
        CASE WHEN ?6=0 THEN id END ASC, CASE WHEN ?6=1 THEN id END DESC \
        LIMIT ?7 OFFSET ?8",
    )
    .bind(q.artist_id)
    .bind(q.recording_id)
    .bind(q.kind)
    .bind(q.has_audio)
    .bind(q.sort.as_str())
    .bind(q.order.descending())
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((items, total))
}

/// A release card computes availability from known local track occurrences.
pub(crate) async fn release(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<ReleaseSummary>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM client_releases WHERE id=?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Counts and artist cards share one read snapshot, including an empty page.
pub(crate) async fn artists(
    pool: &SqlitePool,
    q: &ArtistQuery,
) -> Result<(Vec<ArtistSummary>, i64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let total = sqlx::query_scalar("SELECT count(*) FROM artists")
        .fetch_one(&mut *tx)
        .await?;
    let items = sqlx::query_as(
        "SELECT * FROM client_artists ORDER BY \
        CASE WHEN ?1='name' AND ?2=0 THEN name END COLLATE NOCASE ASC, \
        CASE WHEN ?1='name' AND ?2=1 THEN name END COLLATE NOCASE DESC, \
        CASE WHEN ?2=0 THEN id END ASC, CASE WHEN ?2=1 THEN id END DESC \
        LIMIT ?3 OFFSET ?4",
    )
    .bind(q.sort.as_str())
    .bind(q.order.descending())
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((items, total))
}

/// One artist card without requiring an unbounded client-side join.
pub(crate) async fn artist(
    pool: &SqlitePool,
    id: i64,
) -> Result<Option<ArtistSummary>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM client_artists WHERE id=?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// A track occurrence retains its original fields and gains its recording
/// card.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct TrackSummary {
    /// Existing placement fields including disc, position and recording ID.
    #[serde(flatten)]
    pub(crate) track: ReleaseTrack,
    /// Full path-free recording display/audio summary for this occurrence.
    pub(crate) recording: RecordingSummary,
}

/// Preserve occurrence order and duplicates using two batched reads, never
/// N+1.
pub(crate) async fn tracks(
    pool: &SqlitePool,
    release: i64,
    limit: i64,
    offset: i64,
) -> Result<(Vec<TrackSummary>, i64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let total = sqlx::query_scalar(
        "SELECT count(*) FROM release_tracks WHERE release_id=?",
    )
    .bind(release)
    .fetch_one(&mut *tx)
    .await?;
    let tracks: Vec<ReleaseTrack> = sqlx::query_as(
        "SELECT * FROM release_tracks \
        WHERE release_id=? ORDER BY disc_number,position,id LIMIT ? OFFSET ?",
    )
    .bind(release)
    .bind(limit)
    .bind(offset)
    .fetch_all(&mut *tx)
    .await?;
    let ids: Vec<_> = tracks.iter().map(|t| t.recording_id).collect();
    let recordings: Vec<RecordingSummary> = sqlx::query_as(
        "SELECT * FROM client_recordings \
         WHERE id IN (SELECT value FROM json_each(?))",
    )
    .bind(Json(ids))
    .fetch_all(&mut *tx)
    .await?;
    let lookup: std::collections::HashMap<_, _> =
        recordings.into_iter().map(|r| (r.id, r)).collect();
    let items = tracks
        .into_iter()
        .map(|track| {
            let recording =
                lookup.get(&track.recording_id).cloned().ok_or_else(|| {
                    sqlx::Error::Protocol(
                        "Missing referenced recording".into(),
                    )
                })?;
            Ok(TrackSummary { track, recording })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    tx.commit().await?;
    Ok((items, total))
}

/// Aggregate catalog numbers are one consistent SQL statement, not disk scans.
pub(crate) async fn stats(
    pool: &SqlitePool,
) -> Result<LibraryStats, sqlx::Error> {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM artists) artists, \
        (SELECT count(*) FROM releases) releases, \
        (SELECT count(*) FROM recordings) recordings, \
        (SELECT count(*) FROM client_recordings WHERE has_audio) \
            recordings_with_audio, \
        (SELECT count(*) FROM audio_files f WHERE NOT EXISTS( \
            SELECT 1 FROM file_removals d WHERE d.file_id=f.id \
                AND d.status='pending')) audio_files",
    )
    .fetch_one(pool)
    .await
}
