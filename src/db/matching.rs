//! Transactional directory-local credits, releases, and track associations.
//!
//! Scanner mappings are independent of global display-name uniqueness. Manual
//! entities are never selected by name. Matching is limited to the immediate
//! parent directory; disc subdirectories are separate scopes. Existing paths
//! are not passed here, preserving both legacy imports and manual corrections.

use std::path::Path;

use sqlx::SqliteConnection;

use crate::library::{matching::MatchingTags, metadata::AudioTags};

/// Attach validated source associations within the caller's file transaction.
///
/// Invalid matching tags leave associations absent. Occupied track positions
/// leave that track unlinked, preserving the new file and performer credit.
/// The returned issue must be persisted by the caller in the same transaction.
/// Reused recordings retain their credits; only new recordings receive them.
pub(super) async fn attach(
    connection: &mut SqliteConnection,
    recording_id: i64,
    path: &str,
    source: &AudioTags,
    new_recording: bool,
) -> Result<Option<&'static str>, sqlx::Error> {
    let tags = match MatchingTags::parse(source) {
        Ok(tags) => tags,
        Err(message) => return Ok(Some(message)),
    };
    let Some(directory) = Path::new(path).parent().and_then(Path::to_str)
    else {
        return Ok(Some("Cannot determine the source album directory"));
    };
    if let Some(name) = tags.artist.as_ref().filter(|_| new_recording) {
        let id = artist(connection, directory, name).await?;
        sqlx::query(
            "INSERT INTO recording_artists \
            (recording_id, artist_id, position) VALUES (?, ?, 0)",
        )
        .bind(recording_id)
        .bind(id)
        .execute(&mut *connection)
        .await?;
    }
    let Some(title) = &tags.album else {
        return Ok(None);
    };
    let release_id =
        release(connection, directory, title, tags.album_artist.as_deref())
            .await?;
    if let Some(position) = tags.track {
        let inserted = sqlx::query(
            "INSERT INTO release_tracks \
             (release_id, recording_id, disc_number, position) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(release_id, disc_number, position) DO NOTHING",
        )
        .bind(release_id)
        .bind(recording_id)
        .bind(i64::from(tags.disc))
        .bind(i64::from(position))
        .execute(&mut *connection)
        .await?;
        if inserted.rows_affected() == 0 {
            let occupied: i64 = sqlx::query_scalar(
                "SELECT recording_id FROM release_tracks \
                 WHERE release_id = ? AND disc_number = ? AND position = ?",
            )
            .bind(release_id)
            .bind(i64::from(tags.disc))
            .bind(i64::from(position))
            .fetch_one(connection)
            .await?;
            if occupied == recording_id {
                return Ok(None);
            }
            return Ok(Some(
                "Source release track position is occupied; \
                file imported without a release track",
            ));
        }
    }
    Ok(None)
}

/// Reuse one source credit in this directory, keeping curated names intact.
async fn artist(
    connection: &mut SqliteConnection,
    directory: &str,
    name: &str,
) -> Result<i64, sqlx::Error> {
    let existing = sqlx::query_scalar(
        "SELECT artist_id FROM scan_artist_matches \
         WHERE directory = ? AND source_name = ?",
    )
    .bind(directory)
    .bind(name)
    .fetch_optional(&mut *connection)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = sqlx::query_scalar(
        "INSERT INTO artists (name) VALUES (?) RETURNING id",
    )
    .bind(name)
    .fetch_one(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO scan_artist_matches \
        (directory, source_name, artist_id) VALUES (?, ?, ?)",
    )
    .bind(directory)
    .bind(name)
    .bind(id)
    .execute(connection)
    .await?;
    Ok(id)
}

/// Reuse a local album title/credit pair; absent album credit stays unknown.
///
/// The generic album tag does not imply a release kind, so new releases use
/// `other`. Only explicit album-artist metadata supplies a release credit.
async fn release(
    connection: &mut SqliteConnection,
    directory: &str,
    title: &str,
    album_credit: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let credit = album_credit.unwrap_or("");
    let existing = sqlx::query_scalar(
        "SELECT release_id FROM scan_release_matches \
         WHERE directory = ? AND source_title = ? AND album_credit = ?",
    )
    .bind(directory)
    .bind(title)
    .bind(credit)
    .fetch_optional(&mut *connection)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = sqlx::query_scalar(
        "INSERT INTO releases (title, kind) VALUES (?, 'other') RETURNING id",
    )
    .bind(title)
    .fetch_one(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO scan_release_matches \
        (directory, source_title, album_credit, release_id) \
        VALUES (?, ?, ?, ?)",
    )
    .bind(directory)
    .bind(title)
    .bind(credit)
    .bind(id)
    .execute(&mut *connection)
    .await?;
    if let Some(name) = album_credit {
        let artist_id = artist(connection, directory, name).await?;
        sqlx::query(
            "INSERT INTO release_artists \
            (release_id, artist_id, position) VALUES (?, ?, 0)",
        )
        .bind(id)
        .bind(artist_id)
        .execute(connection)
        .await?;
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    //! Import-level association tests using real SQLite file transactions.

    use super::*;
    use crate::{
        db,
        library::{audio_files::AudioProperties, metadata::ScannedFile},
    };

    /// Prepare a durable catalog and an active job for import assertions.
    async fn setup() -> (tempfile::TempDir, sqlx::SqlitePool, i64) {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        (directory, pool, job.id)
    }

    /// Describe one track with separate recording/release credits.
    fn source(path: &str, track: Option<u32>) -> ScannedFile {
        ScannedFile {
            path: path.into(),
            state: None,
            sha256: None,
            format: "flac".into(),
            tags: AudioTags {
                title: Some("Song".into()),
                artist: Some("Singer".into()),
                album: Some("Album".into()),
                album_artist: Some("Album artist".into()),
                track_number: track,
                ..AudioTags::default()
            },
            properties: AudioProperties::default(),
        }
    }

    /// Different containers with explicit identity share one recording/slot.
    #[tokio::test]
    async fn links_formats_by_explicit_identity_without_rewriting_credits() {
        let (directory, pool, job) = setup().await;
        let id = "ebf79ba5-085e-48d2-9eb8-2d992fbf0f6d";
        let mut first = source("/music/album/one.flac", Some(1));
        first.tags.recording_mbid = Some(id.into());
        db::indexing::store(&pool, job, first).await.unwrap();
        sqlx::query("UPDATE recordings SET title = 'Curated title'")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let mut second = source("/music/album/one.mp3", Some(1));
        second.format = "mp3".into();
        second.tags.recording_mbid = Some(id.to_uppercase());
        second.tags.artist = Some("Different source credit".into());
        db::indexing::store(&pool, job, second).await.unwrap();
        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM recordings), \
             (SELECT COUNT(*) FROM audio_files), \
             (SELECT COUNT(*) FROM release_tracks)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 2, 1));
        let recordings =
            db::recordings::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(recordings[0].title, "Curated title");
        let artist = db::artists::find(&pool, recordings[0].artist_ids[0])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(artist.name, "Singer");
        assert_eq!(
            db::scans::find(&pool, job).await.unwrap().unwrap().errors,
            0
        );
        let mut other = source("/other/release/track.flac", Some(4));
        other.tags.recording_mbid = Some(id.into());
        db::indexing::store(&pool, job, other).await.unwrap();
        assert_eq!(
            db::recordings::fixtures::list(&pool, 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            db::releases::fixtures::list(&pool, 50, 0)
                .await
                .unwrap()
                .len(),
            2
        );
        pool.close().await;
    }

    /// Equal titles do not override distinct IDs or links of known paths.
    #[tokio::test]
    async fn distinct_identities_and_known_path_links_are_preserved() {
        let (_directory, pool, job) = setup().await;
        let a = "ebf79ba5-085e-48d2-9eb8-2d992fbf0f6d";
        let b = "0383dadf-2a4e-4d10-a46a-e9e041da8eb3";
        for (path, id) in [("/a.flac", a), ("/b.mp3", b)] {
            let mut file = source(path, None);
            file.tags.recording_mbid = Some(id.into());
            db::indexing::store(&pool, job, file).await.unwrap();
        }
        let before: Vec<(String, i64)> = sqlx::query_as(
            "SELECT path, recording_id FROM audio_files ORDER BY path",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_ne!(before[0].1, before[1].1);
        let mut changed = source("/b.mp3", None);
        changed.tags.recording_mbid = Some(a.into());
        db::indexing::store(&pool, job, changed).await.unwrap();
        let after: Vec<(String, i64)> = sqlx::query_as(
            "SELECT path, recording_id FROM audio_files ORDER BY path",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(before, after);
        pool.close().await;
    }

    /// Invalid IDs report an issue but do not drop otherwise valid files.
    #[tokio::test]
    async fn invalid_recording_identity_keeps_an_independent_file() {
        let (_directory, pool, job) = setup().await;
        let mut file = source("/music/song.flac", Some(1));
        file.tags.recording_mbid = Some("not-an-id".into());
        db::indexing::store(&pool, job, file).await.unwrap();
        let job = db::scans::find(&pool, job).await.unwrap().unwrap();
        assert_eq!(job.imported, 1);
        assert_eq!(job.errors, 1);
        pool.close().await;
    }

    /// Failure after resolving identity rolls back the new identity mapping.
    #[tokio::test]
    async fn identity_mapping_rolls_back_with_failed_file() {
        let (_directory, pool, job) = setup().await;
        sqlx::query(
            "CREATE TRIGGER reject_file BEFORE INSERT ON audio_files \
            BEGIN SELECT RAISE(ABORT, 'failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut file = source("/music/song.flac", Some(1));
        file.tags.recording_mbid =
            Some("ebf79ba5-085e-48d2-9eb8-2d992fbf0f6d".into());
        assert!(db::indexing::store(&pool, job, file).await.is_err());
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM scan_recording_matches")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
        pool.close().await;
    }

    /// Compilation performers share a release and keep independent credits.
    #[tokio::test]
    async fn assembles_a_compilation_with_explicit_disc_and_track_positions() {
        let (_directory, pool, job) = setup().await;
        let first = source("/music/album/one.flac", Some(1));
        let mut second = source("/music/album/two.flac", Some(2));
        second.tags.artist = Some("Guest".into());
        let mut third = source("/music/album/three.flac", Some(1));
        third.tags.disc_number = Some(2);
        for file in [first, second, third] {
            db::indexing::store(&pool, job, file).await.unwrap();
        }
        let releases =
            db::releases::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].title, "Album");
        let release = &releases[0];
        let album_artist = db::artists::find(&pool, release.artist_ids[0])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(album_artist.name, "Album artist");
        let tracks =
            db::release_tracks::fixtures::list(&pool, release.id, 50, 0)
                .await
                .unwrap();
        assert_eq!(
            tracks
                .iter()
                .map(|track| (track.disc_number, track.position))
                .collect::<Vec<_>>(),
            [(1, 1), (1, 2), (2, 1)]
        );
        let recordings =
            db::recordings::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(recordings[0].artist_ids, recordings[2].artist_ids);
        assert_ne!(recordings[0].artist_ids, recordings[1].artist_ids);
        assert_ne!(recordings[0].artist_ids, release.artist_ids);
        let artists = db::artists::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(artists.len(), 3);
        pool.close().await;
    }

    /// Display-name equality cannot match manual entities or another folder.
    #[tokio::test]
    async fn isolates_directories_album_credits_and_manual_entities() {
        let (_directory, pool, job) = setup().await;
        sqlx::query("INSERT INTO artists (id, name) VALUES (100, 'Singer')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO releases (id, title, kind) \
            VALUES (100, 'Album', 'album')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let first = source("/music/one/song.flac", Some(1));
        let second = source("/music/two/song.flac", Some(1));
        let mut third = source("/music/one/other.flac", Some(1));
        third.tags.album_artist = Some("Another album artist".into());
        for file in [first, second, third] {
            db::indexing::store(&pool, job, file).await.unwrap();
        }
        let releases =
            db::releases::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(releases.len(), 4);
        let recordings =
            db::recordings::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_ne!(recordings[0].artist_ids, recordings[1].artist_ids);
        assert_eq!(recordings[0].artist_ids, recordings[2].artist_ids);
        assert_ne!(recordings[0].artist_ids, [100]);
        let tracks: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM release_tracks WHERE release_id = 100",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tracks, 0);
        pool.close().await;
    }

    /// Missing credits and numbers never invent performers or track positions.
    #[tokio::test]
    async fn handles_partial_tags_without_guessing_associations() {
        let (_directory, pool, job) = setup().await;
        let mut file = source("/music/partial/no-number.flac", None);
        file.tags.artist = None;
        file.tags.album_artist = None;
        db::indexing::store(&pool, job, file).await.unwrap();
        let mut standalone = source("/music/partial/standalone.flac", Some(8));
        standalone.tags.album = None;
        db::indexing::store(&pool, job, standalone).await.unwrap();
        let releases =
            db::releases::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(releases.len(), 1);
        assert!(releases[0].artist_ids.is_empty());
        assert!(matches!(
            releases[0].kind,
            crate::library::releases::ReleaseKind::Other
        ));
        assert!(
            db::release_tracks::fixtures::list(&pool, releases[0].id, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        let recordings =
            db::recordings::fixtures::list(&pool, 50, 0).await.unwrap();
        assert!(recordings[0].artist_ids.is_empty());
        assert_eq!(recordings[1].artist_ids.len(), 1);
        pool.close().await;
    }

    /// Conflicting slots keep both audio files and report the unlinked one.
    #[tokio::test]
    async fn preserves_files_and_reports_occupied_track_positions() {
        let (_directory, pool, job) = setup().await;
        for path in ["/music/album/song.flac", "/music/album/song.mp3"] {
            db::indexing::store(&pool, job, source(path, Some(1)))
                .await
                .unwrap();
        }
        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM audio_files), \
             (SELECT COUNT(*) FROM recordings), \
             (SELECT COUNT(*) FROM release_tracks)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counts, (2, 2, 1));
        let job = db::scans::find(&pool, job).await.unwrap().unwrap();
        assert_eq!(job.imported, 2);
        assert_eq!(job.errors, 1);
        let errors = db::scans::errors(&pool, job.id, 50, 0).await.unwrap();
        assert_eq!(errors[0].path, "/music/album/song.mp3");
        assert!(errors[0].message.contains("position is occupied"));
        pool.close().await;
    }

    /// Invalid optional tags preserve source observations and report an issue.
    #[tokio::test]
    async fn keeps_audio_when_matching_metadata_is_invalid() {
        let (_directory, pool, job) = setup().await;
        let mut file = source("/music/album/song.flac", Some(1));
        file.tags.album_artist = Some("bad\ncredit".into());
        db::indexing::store(&pool, job, file).await.unwrap();
        let recordings =
            db::recordings::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(recordings.len(), 1);
        assert!(recordings[0].artist_ids.is_empty());
        assert!(
            db::releases::fixtures::list(&pool, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        let file = db::audio_files::list(&pool, recordings[0].id, 50, 0)
            .await
            .unwrap()
            .remove(0);
        let tags = db::indexing::tags(&pool, recordings[0].id, file.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tags.album_artist.as_deref(), Some("bad\ncredit"));
        let job = db::scans::find(&pool, job).await.unwrap().unwrap();
        assert_eq!(job.imported, 1);
        assert_eq!(job.errors, 1);
        pool.close().await;
    }

    /// A report failure rolls back new credits, recording, and progress too.
    #[tokio::test]
    async fn rolls_back_associations_when_issue_persistence_fails() {
        let (_directory, pool, job) = setup().await;
        db::indexing::store(
            &pool,
            job,
            source("/music/album/one.flac", Some(1)),
        )
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER reject_issue BEFORE INSERT \
            ON library_scan_errors BEGIN SELECT RAISE(ABORT, 'failure'); END",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut other = source("/music/album/other.flac", Some(1));
        other.tags.artist = Some("New credit".into());
        assert!(matches!(
            db::indexing::store(&pool, job, other).await,
            Err(db::indexing::IndexError::Storage(_))
        ));
        let counts: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM artists), \
             (SELECT COUNT(*) FROM recordings), \
             (SELECT COUNT(*) FROM audio_files), \
             (SELECT COUNT(*) FROM release_tracks)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counts, (2, 1, 1, 1));
        let job = db::scans::find(&pool, job).await.unwrap().unwrap();
        assert_eq!(job.imported, 1);
        assert_eq!(job.errors, 0);
        pool.close().await;
    }

    /// Source keys survive restarts and curated name/track corrections.
    #[tokio::test]
    async fn reuses_durable_matches_and_preserves_manual_corrections() {
        let (directory, pool, job) = setup().await;
        db::indexing::store(
            &pool,
            job,
            source("/music/album/one.flac", Some(1)),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE artists SET name = 'Curated credit'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE releases SET title = 'Curated album'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE recordings SET title = 'Curated song'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE release_tracks SET disc_number = 7")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        db::indexing::store(
            &pool,
            job,
            source("/music/album/one.flac", Some(1)),
        )
        .await
        .unwrap();
        db::indexing::store(
            &pool,
            job,
            source("/music/album/two.flac", Some(2)),
        )
        .await
        .unwrap();
        let releases =
            db::releases::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].title, "Curated album");
        let recordings =
            db::recordings::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(recordings[0].title, "Curated song");
        assert_eq!(recordings[0].artist_ids, recordings[1].artist_ids);
        let tracks =
            db::release_tracks::fixtures::list(&pool, releases[0].id, 50, 0)
                .await
                .unwrap();
        assert_eq!(
            tracks
                .iter()
                .map(|track| (track.disc_number, track.position))
                .collect::<Vec<_>>(),
            [(1, 2), (7, 1)]
        );
        let artists = db::artists::fixtures::list(&pool, 50, 0).await.unwrap();
        assert_eq!(artists.len(), 2);
        assert!(artists.iter().all(|artist| artist.name == "Curated credit"));
        let job = db::scans::find(&pool, job).await.unwrap().unwrap();
        assert_eq!(job.imported, 2);
        assert_eq!(job.refreshed, 1);
        pool.close().await;
    }
}
