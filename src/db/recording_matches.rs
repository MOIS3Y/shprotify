//! Explicit recording identities for new paths, independent of encoding.

use sqlx::SqliteConnection;
use std::path::Path;

use super::indexing::IndexError;
use crate::library::{
    matching, metadata::AudioTags, recordings::NewRecording,
};

/// Resolve or create a recording, returning ownership and an optional issue.
///
/// Only scanner-owned identity mappings are searched; existing file links are
/// handled before this function. Reused recordings keep their title/credits.
pub(super) async fn resolve(
    connection: &mut SqliteConnection,
    path: &str,
    tags: &AudioTags,
) -> Result<(i64, bool, Option<&'static str>), IndexError> {
    let (mbid, issue) =
        match matching::recording_id(tags.recording_mbid.as_deref()) {
            Ok(id) => (id, None),
            Err(message) => (None, Some(message)),
        };
    if let Some(id) = &mbid {
        let found = sqlx::query_scalar(
            "SELECT recording_id FROM scan_recording_matches WHERE mbid = ?",
        )
        .bind(id)
        .fetch_optional(&mut *connection)
        .await?;
        if let Some(recording) = found {
            return Ok((recording, false, issue));
        }
    }
    let fallback = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled");
    let title = tags
        .title
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback);
    let input =
        NewRecording::new(title, vec![]).map_err(IndexError::Invalid)?;
    let recording: i64 = sqlx::query_scalar(
        "INSERT INTO recordings (title) VALUES (?) RETURNING id",
    )
    .bind(input.title)
    .fetch_one(&mut *connection)
    .await?;
    if let Some(id) = mbid {
        sqlx::query(
            "INSERT INTO scan_recording_matches \
            (mbid, recording_id) VALUES (?, ?)",
        )
        .bind(id)
        .bind(recording)
        .execute(connection)
        .await?;
    }
    Ok((recording, true, issue))
}
