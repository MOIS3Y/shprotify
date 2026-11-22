//! SQLite pool initialization and embedded startup migrations.
//!
//! Database paths are filesystem paths, not SQLite URLs. Parent directories
//! must exist. Every connection enables foreign keys and waits up to five
//! seconds for a SQLite write lock; WAL allows concurrent readers.

pub(crate) mod artists;
pub(crate) mod audio_files;
pub(crate) mod indexing;
mod matching;
pub(crate) mod recordings;
pub(crate) mod release_tracks;
pub(crate) mod releases;
pub(crate) mod scans;

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
        assert_eq!(versions, [1, 2, 3, 4, 5, 6, 7]);
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
        let job = scans::create(&pool, &[]).await.unwrap();
        indexing::store(
            &pool,
            job.id,
            crate::library::metadata::ScannedFile {
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
