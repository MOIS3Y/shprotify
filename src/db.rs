//! SQLite pool initialization and embedded startup migrations.
//!
//! Database paths are filesystem paths, not SQLite URLs. Parent directories
//! must exist. Every connection enables foreign keys and waits up to five
//! seconds for a SQLite write lock; WAL allows concurrent readers.

pub(crate) mod artists;
pub(crate) mod audio_files;
pub(crate) mod duplicates;
pub(crate) mod history;
pub(crate) mod indexing;
mod matching;
pub(crate) mod playlists;
pub(crate) mod reactions;
mod recording_matches;
pub(crate) mod recordings;
pub(crate) mod release_tracks;
pub(crate) mod releases;
pub(crate) mod scans;
pub(crate) mod search;
pub(crate) mod sessions;
pub(crate) mod users;

use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

/// Migrations embedded at compile time; no runtime SQL directory is needed.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// Open or create the database and apply pending migrations before returning.
///
/// The pool keeps at least one connection and opens at most five. Connection
/// acquisition times out after ten seconds. Migration failures close the pool
/// and prevent HTTP startup; applied migration checksums are validated.
pub(crate) async fn connect(path: &Path) -> Result<SqlitePool> {
    connect_with_migrations(path, &MIGRATOR).await
}

/// Initialize a pool using a migration set supplied by startup or a test.
///
/// Close connections on migration failure rather than returning a partially
/// initialized database to the caller.
async fn connect_with_migrations(
    path: &Path,
    migrator: &Migrator,
) -> Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .min_connections(1)
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(options)
        .await
        .with_context(|| {
            format!("Failed to open SQLite database {}", path.display())
        })?;

    if let Err(error) = migrator.run(&pool).await {
        pool.close().await;
        return Err(error).with_context(|| {
            format!("Failed to migrate SQLite database {}", path.display())
        });
    }
    Ok(pool)
}

#[cfg(test)]
mod tests {
    //! File-backed SQLite behavior and migration recovery checks.

    use std::fs;

    use tempfile::TempDir;

    use super::*;

    /// Upgrading adds no publication and preserves old versions and contents.
    #[tokio::test]
    async fn upgrades_existing_playlists_as_private_without_changing_versions()
    {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let previous = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 16)
                .cloned()
                .collect(),
        );
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        users::create(&pool, "legacy", &crate::auth::digest("legacy-key"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (id,title) VALUES (1,'Track')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO playlists (id,user_id,name,version) \
            VALUES (1,1,'Legacy',7)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO playlist_tracks (playlist_id,position,recording_id) \
            VALUES (1,1,1),(1,2,1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let times: (String, String) = sqlx::query_as(
            "SELECT created_at, updated_at FROM playlists WHERE id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let playlist = playlists::find(&pool, 1, 1).await.unwrap().unwrap();
        assert!(!playlist.is_public);
        assert_eq!(playlist.version, 7);
        assert_eq!((playlist.created_at, playlist.updated_at), times);
        assert_eq!(
            playlists::tracks(&pool, 1, 1, 50, 0)
                .await
                .unwrap()
                .items
                .len(),
            2
        );
        assert!(playlists::published(&pool, 50, 0).await.unwrap().is_empty());
        assert!(
            playlists::publish(&pool, 1, 1, 7, true)
                .await
                .unwrap()
                .is_public
        );
        pool.close().await;
    }

    /// Existing preferences/history survive the initially empty playlist
    /// tables.
    #[tokio::test]
    async fn upgrades_history_to_playlists_without_creating_favorites() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let previous = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 15)
                .cloned()
                .collect(),
        );
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        users::create(&pool, "legacy", &crate::auth::digest("legacy-key"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (id, title) VALUES (1,'Track')")
            .execute(&pool)
            .await
            .unwrap();
        reactions::set(
            &pool,
            1,
            1,
            Some(crate::library::reactions::Reaction::Like),
        )
        .await
        .unwrap();
        history::record(
            &pool,
            1,
            crate::audio::history::NewPlayback::new(
                "existing".into(),
                1,
                0,
                100,
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        assert!(playlists::list(&pool, 1, 50, 0).await.unwrap().is_empty());
        assert_eq!(
            reactions::favorites(&pool, 1, 50, 0).await.unwrap().len(),
            1
        );
        assert_eq!(history::list(&pool, 1, 50, 0).await.unwrap().len(), 1);
        assert!(
            users::authenticate(&pool, &crate::auth::digest("legacy-key"))
                .await
                .unwrap()
                .is_some()
        );
        pool.close().await;
    }

    /// Existing credentials/preferences survive adding an initially empty log.
    #[tokio::test]
    async fn upgrades_existing_accounts_to_playback_history() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let previous = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 14)
                .cloned()
                .collect(),
        );
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        users::create(&pool, "legacy", &crate::auth::digest("legacy-key"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (id, title) VALUES (1,'Track')")
            .execute(&pool)
            .await
            .unwrap();
        reactions::set(
            &pool,
            1,
            1,
            Some(crate::library::reactions::Reaction::Like),
        )
        .await
        .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        assert!(
            users::authenticate(&pool, &crate::auth::digest("legacy-key"))
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            reactions::favorites(&pool, 1, 50, 0).await.unwrap().len(),
            1
        );
        assert!(history::list(&pool, 1, 50, 0).await.unwrap().is_empty());
        pool.close().await;
    }

    /// Existing entities become searchable when FTS is introduced.
    #[tokio::test]
    async fn upgrades_catalog_and_indexes_existing_entities() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let previous = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 10)
                .cloned()
                .collect(),
        );
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        sqlx::query("INSERT INTO artists (name) VALUES ('Legacy')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (title) VALUES ('Legacy')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO releases (title, kind) \
            VALUES ('Legacy', 'other')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let query = crate::library::search::phrase("leg").unwrap();
        assert_eq!(
            search::find(&pool, &query, None, 50, 0)
                .await
                .unwrap()
                .len(),
            3
        );
        sqlx::query("UPDATE recordings SET title = 'Changed'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE releases SET title = 'Changed'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            search::find(&pool, &query, None, 50, 0)
                .await
                .unwrap()
                .len(),
            1
        );
        let changed = crate::library::search::phrase("changed").unwrap();
        assert_eq!(
            search::find(&pool, &changed, None, 50, 0)
                .await
                .unwrap()
                .len(),
            2
        );
        sqlx::query("DELETE FROM recordings")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM releases")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            search::find(&pool, &changed, None, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        pool.close().await;
    }

    /// Adding personal data preserves legacy catalog identities and search.
    #[tokio::test]
    async fn upgrades_legacy_catalog_to_personal_reactions() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let previous = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 11)
                .cloned()
                .collect(),
        );
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        sqlx::query("INSERT INTO recordings (id, title) VALUES (1, 'Legacy')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let user = users::create(&pool, "alice", &crate::auth::digest("key"))
            .await
            .unwrap();
        assert!(
            reactions::find(&pool, user.id, 1)
                .await
                .unwrap()
                .unwrap()
                .reaction
                .is_none()
        );
        assert!(
            reactions::favorites(&pool, user.id, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            recordings::find(&pool, 1).await.unwrap().unwrap().title,
            "Legacy"
        );
        pool.close().await;
    }

    /// Existing keys/data survive role migration without implicit privileges.
    #[tokio::test]
    async fn upgrades_existing_accounts_as_members_and_preserves_keys() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let old = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 12)
                .cloned()
                .collect(),
        );
        let pool = connect_with_migrations(&path, &old).await.unwrap();
        sqlx::query("INSERT INTO users (id, name) VALUES (1, 'admin')")
            .execute(&pool)
            .await
            .unwrap();
        let digest = crate::auth::digest("legacy-key");
        sqlx::query("INSERT INTO user_keys (user_id, digest) VALUES (1, ?)")
            .bind(&digest)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (id, title) VALUES (1, 'Track')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO recording_reactions \
            (user_id, recording_id, reaction, changed_at) \
            VALUES (1, 1, 'like', '2000-01-01T00:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let user = users::authenticate(&pool, &digest).await.unwrap().unwrap();
        assert_eq!(user.role, crate::auth::Role::Member);
        assert_eq!(user.id, 1);
        users::set_role(&pool, "admin", crate::auth::Role::Admin)
            .await
            .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        assert_eq!(
            users::authenticate(&pool, &digest)
                .await
                .unwrap()
                .unwrap()
                .role,
            crate::auth::Role::Admin
        );
        assert_eq!(
            reactions::favorites(&pool, 1, 50, 0).await.unwrap().len(),
            1
        );
        pool.close().await;
    }

    /// Resolve one test migration without adding test tables to the binary.
    async fn migrator(directory: &Path, sql: &str) -> Migrator {
        fs::write(directory.join("1_test.sql"), sql).unwrap();
        Migrator::new(directory).await.unwrap()
    }

    /// Every pooled connection receives the required SQLite pragmas.
    #[tokio::test]
    async fn creates_database_and_configures_connections() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("music library.db");
        let pool = connect(&path).await.unwrap();
        assert!(path.is_file());

        // Hold the first connection to force acquisition of another one.
        let first = pool.acquire().await.unwrap();
        let mut second = pool.acquire().await.unwrap();
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut *second)
            .await
            .unwrap();
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut *second)
            .await
            .unwrap();
        let timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&mut *second)
            .await
            .unwrap();
        assert_eq!(foreign_keys, 1);
        assert_eq!(journal, "wal");
        assert_eq!(timeout, 5000);

        drop(first);
        drop(second);
        pool.close().await;
    }

    /// Databases created before catalog migrations upgrade on startup.
    #[tokio::test]
    async fn upgrades_existing_database_with_no_application_tables() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        let empty = Migrator::with_migrations(Vec::new());
        let pool = connect_with_migrations(&path, &empty).await.unwrap();
        pool.close().await;

        let pool = connect(&path).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artists")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let versions: Vec<i64> = sqlx::query_scalar(
            "SELECT version FROM _sqlx_migrations \
             WHERE success = 1 ORDER BY version",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(versions, (1..=17).collect::<Vec<_>>());
        pool.close().await;
    }

    /// New matching tables preserve previously indexed paths and identities.
    #[tokio::test]
    async fn upgrades_scan_catalog_without_rewriting_legacy_associations() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        let previous = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 6)
                .cloned()
                .collect(),
        );
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        sqlx::query("INSERT INTO recordings (id, title) VALUES (1, 'Legacy')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO audio_files (id, recording_id, path, format) \
            VALUES (1, 1, '/music/legacy.flac', 'flac')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO audio_file_tags (file_id, tags) \
            VALUES (1, '{\"artist\":\"Source credit\"}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let row: (i64, String) = sqlx::query_as(
            "SELECT f.recording_id, r.title FROM audio_files f \
             JOIN recordings r ON r.id = f.recording_id WHERE f.id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, (1, "Legacy".into()));
        let matches: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM scan_artist_matches), \
             (SELECT COUNT(*) FROM scan_release_matches)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(matches, (0, 0));
        let job = scans::create(&pool, &[], false).await.unwrap();
        indexing::store(
            &pool,
            job.id,
            crate::library::metadata::ScannedFile {
                state: None,
                sha256: None,
                path: "/music/legacy.flac".into(),
                format: "flac".into(),
                tags: crate::library::metadata::AudioTags {
                    title: Some("Changed source".into()),
                    artist: Some("Source credit".into()),
                    album: Some("Album".into()),
                    track_number: Some(1),
                    ..Default::default()
                },
                properties: Default::default(),
            },
        )
        .await
        .unwrap();
        assert!(
            recordings::find(&pool, 1)
                .await
                .unwrap()
                .unwrap()
                .artist_ids
                .is_empty()
        );
        assert!(releases::list(&pool, 50, 0).await.unwrap().is_empty());
        pool.close().await;
    }

    /// Legacy files probe once; old job snapshots gain safe policy defaults.
    #[tokio::test]
    async fn upgrades_catalog_to_incremental_scans_without_false_cache_hits() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let audio = directory.path().join("song.wav");
        fs::write(&audio, crate::library::metadata::tests::wave()).unwrap();
        let previous = Migrator::with_migrations(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= 7)
                .cloned()
                .collect(),
        );
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        sqlx::query("INSERT INTO recordings (id, title) VALUES (1, 'Legacy')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO audio_files (recording_id, path, format) \
            VALUES (1, ?, 'wav')",
        )
        .bind(audio.to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO library_scans (status, roots, refreshed) \
            VALUES ('completed', '[]', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let old = scans::find(&pool, 1).await.unwrap().unwrap();
        assert!(!old.force);
        assert_eq!(old.unchanged, 0);
        assert_eq!(old.refreshed, 1);
        let state =
            crate::library::file_state::FileState::read(&audio).unwrap();
        assert!(
            !indexing::unchanged(&pool, audio.to_str().unwrap(), &state)
                .await
                .unwrap()
        );
        let job = scans::create(&pool, &[], false).await.unwrap();
        indexing::store(
            &pool,
            job.id,
            crate::library::metadata::read(&audio).unwrap().unwrap(),
        )
        .await
        .unwrap();
        assert!(
            indexing::unchanged(&pool, audio.to_str().unwrap(), &state)
                .await
                .unwrap()
        );
        assert_eq!(
            recordings::find(&pool, 1).await.unwrap().unwrap().title,
            "Legacy"
        );
        let job = scans::find(&pool, job.id).await.unwrap().unwrap();
        assert_eq!(job.imported, 0);
        assert_eq!(job.refreshed, 1);
        pool.close().await;
    }

    /// Upgrading the artist-only schema preserves its existing catalog data.
    #[tokio::test]
    async fn upgrades_artist_catalog_to_release_schema() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        let first = migrator(
            directory.path(),
            include_str!("../migrations/1_create_artists.sql"),
        )
        .await;
        let pool = connect_with_migrations(&path, &first).await.unwrap();
        sqlx::query("INSERT INTO artists (name) VALUES ('Existing')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let pool = connect(&path).await.unwrap();
        let name: String = sqlx::query_scalar("SELECT name FROM artists")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Existing");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM releases")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        pool.close().await;
    }

    /// The recording migration preserves existing releases and their credits.
    #[tokio::test]
    async fn upgrades_release_catalog_to_recording_schema() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        fs::write(
            directory.path().join("1_create_artists.sql"),
            include_str!("../migrations/1_create_artists.sql"),
        )
        .unwrap();
        fs::write(
            directory.path().join("2_create_releases.sql"),
            include_str!("../migrations/2_create_releases.sql"),
        )
        .unwrap();
        let previous = Migrator::new(directory.path()).await.unwrap();
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        let artist = artists::create(
            &pool,
            &crate::library::ArtistName::parse("Existing").unwrap(),
        )
        .await
        .unwrap();
        let release = releases::create(
            &pool,
            crate::library::releases::NewRelease::new(
                "Existing release",
                crate::library::releases::ReleaseKind::Album,
                vec![artist.id],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        pool.close().await;

        let pool = connect(&path).await.unwrap();
        let fetched =
            releases::find(&pool, release.id).await.unwrap().unwrap();
        assert_eq!(fetched.title, release.title);
        assert_eq!(fetched.artist_ids, [artist.id]);
        assert!(recordings::list(&pool, 50, 0).await.unwrap().is_empty());
        pool.close().await;
    }

    /// Adding track placements leaves the existing catalog identities intact.
    #[tokio::test]
    async fn upgrades_recording_catalog_to_track_schema() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        for (name, sql) in [
            (
                "1_create_artists.sql",
                include_str!("../migrations/1_create_artists.sql"),
            ),
            (
                "2_create_releases.sql",
                include_str!("../migrations/2_create_releases.sql"),
            ),
            (
                "3_create_recordings.sql",
                include_str!("../migrations/3_create_recordings.sql"),
            ),
        ] {
            fs::write(directory.path().join(name), sql).unwrap();
        }
        let previous = Migrator::new(directory.path()).await.unwrap();
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        let recording = recordings::create(
            &pool,
            crate::library::recordings::NewRecording::new("Existing", vec![])
                .unwrap(),
        )
        .await
        .unwrap();
        let release = releases::create(
            &pool,
            crate::library::releases::NewRelease::new(
                "Existing album",
                crate::library::releases::ReleaseKind::Album,
                vec![],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        pool.close().await;

        let pool = connect(&path).await.unwrap();
        assert_eq!(
            recordings::find(&pool, recording.id)
                .await
                .unwrap()
                .unwrap()
                .title,
            "Existing"
        );
        assert!(
            release_tracks::list(&pool, release.id, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        pool.close().await;
    }

    /// Adding file metadata preserves recordings and existing track positions.
    #[tokio::test]
    async fn upgrades_track_catalog_to_audio_file_schema() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        for (name, sql) in [
            (
                "1_create_artists.sql",
                include_str!("../migrations/1_create_artists.sql"),
            ),
            (
                "2_create_releases.sql",
                include_str!("../migrations/2_create_releases.sql"),
            ),
            (
                "3_create_recordings.sql",
                include_str!("../migrations/3_create_recordings.sql"),
            ),
            (
                "4_create_release_tracks.sql",
                include_str!("../migrations/4_create_release_tracks.sql"),
            ),
        ] {
            fs::write(directory.path().join(name), sql).unwrap();
        }
        let previous = Migrator::new(directory.path()).await.unwrap();
        let pool = connect_with_migrations(&path, &previous).await.unwrap();
        let recording = recordings::create(
            &pool,
            crate::library::recordings::NewRecording::new("Existing", vec![])
                .unwrap(),
        )
        .await
        .unwrap();
        let release = releases::create(
            &pool,
            crate::library::releases::NewRelease::new(
                "Existing album",
                crate::library::releases::ReleaseKind::Album,
                vec![],
            )
            .unwrap(),
        )
        .await
        .unwrap();
        let track = release_tracks::create(
            &pool,
            release.id,
            crate::library::release_tracks::NewReleaseTrack::new(
                recording.id,
                2,
                3,
            )
            .unwrap(),
        )
        .await
        .unwrap();
        pool.close().await;

        let pool = connect(&path).await.unwrap();
        let stored = release_tracks::find(&pool, release.id, track.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.recording_id, recording.id);
        assert_eq!((stored.disc_number, stored.position), (2, 3));
        assert!(
            audio_files::list(&pool, recording.id, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        pool.close().await;
    }

    /// Reopening an initialized database preserves data and migration history.
    #[tokio::test]
    async fn migrations_run_once_and_data_survives_reopening() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        let migrations = migrator(
            directory.path(),
            "CREATE TABLE example (id INTEGER PRIMARY KEY);\n\
             INSERT INTO example (id) VALUES (42);",
        )
        .await;
        let pool = connect_with_migrations(&path, &migrations).await.unwrap();
        pool.close().await;

        let pool = connect_with_migrations(&path, &migrations).await.unwrap();
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM example")
            .fetch_all(&pool)
            .await
            .unwrap();
        let applied: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM _sqlx_migrations WHERE success = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(ids, [42]);
        assert_eq!(applied, 1);
        pool.close().await;
    }

    /// A failed migration rolls back its SQL and does not poison the pool.
    #[tokio::test]
    async fn migration_failure_rolls_back_and_reports_path() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        let migrations = migrator(
            directory.path(),
            "CREATE TABLE example (id INTEGER); INVALID SQL;",
        )
        .await;
        let error = connect_with_migrations(&path, &migrations)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(&path.display().to_string()));
        assert!(error.to_string().contains("Failed to migrate"));

        let pool = connect(&path).await.unwrap();
        let tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'example'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tables, 0);
        pool.close().await;
    }

    /// Editing an already applied migration is rejected on the next startup.
    #[tokio::test]
    async fn changed_migration_is_rejected() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        let original = migrator(directory.path(), "SELECT 1;").await;
        let pool = connect_with_migrations(&path, &original).await.unwrap();
        pool.close().await;

        let changed = migrator(directory.path(), "SELECT 2;").await;
        let error =
            connect_with_migrations(&path, &changed).await.unwrap_err();
        assert!(format!("{error:#}").contains("previously applied"));
    }

    /// Missing parent directories are reported without creating them.
    #[tokio::test]
    async fn missing_parent_directory_is_an_error() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("missing/library.db");
        let error = connect(&path).await.unwrap_err();
        assert!(error.to_string().contains(&path.display().to_string()));
        assert!(!path.parent().unwrap().exists());
    }

    /// Non-SQLite files are rejected rather than replaced or truncated.
    #[tokio::test]
    async fn invalid_database_file_is_preserved() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("library.db");
        fs::write(&path, "not a database").unwrap();
        assert!(connect(&path).await.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "not a database");
    }
}
