//! SQLite pool initialization and embedded startup migrations.
//!
//! Database paths are filesystem paths, not SQLite URLs. Parent directories
//! must exist. Every connection enables foreign keys and waits up to five
//! seconds for a SQLite write lock; WAL allows concurrent readers.

pub(crate) mod artists;
pub(crate) mod audio_files;
pub(crate) mod automation;
pub(crate) mod browse;
pub(crate) mod discovery;
pub(crate) mod download_import;
pub(crate) mod downloads;
pub(crate) mod duplicates;
pub(crate) mod history;
pub(crate) mod indexing;
mod matching;
pub(crate) mod media;
pub(crate) mod metadata;
pub(crate) mod playlists;
pub(crate) mod queue;
pub(crate) mod reactions;
mod recording_matches;
pub(crate) mod recordings;
pub(crate) mod release_tracks;
pub(crate) mod releases;
pub(crate) mod removals;
pub(crate) mod scans;
pub(crate) mod search;
pub(crate) mod sessions;
pub(crate) mod taste;
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

    /// Reopening preserves feeds and queued work without enabling schedules.
    #[tokio::test]
    async fn reopening_preserves_work_without_enabling_schedules() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("catalog.db");
        let pool = connect(&path).await.unwrap();
        users::create(&pool, "listener", &crate::auth::digest("listener"))
            .await
            .unwrap();
        let request = crate::discovery::FeedRequest::Personal {};
        let feed = discovery::admit(&pool, 1, &request, 0).await.unwrap();
        discovery::complete(
            &pool,
            &feed,
            crate::discovery::GeneratedFeed {
                title: "Existing".into(),
                source_url: None,
                items: vec![],
                warnings: vec![],
            },
        )
        .await
        .unwrap();
        let target = downloads::target(
            &pool,
            1,
            crate::downloads::SearchRequest::Query {
                title: "Pending".into(),
                artists: vec![],
            },
        )
        .await
        .unwrap();
        let job = downloads::enqueue(
            &pool,
            &crate::config::DownloadsConfig::default(),
            1,
            &target,
            Some("abcdefghijk"),
            None,
        )
        .await
        .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let config = crate::config::AutomationConfig::default();
        let schedule = automation::get(
            &pool,
            &config,
            1,
            crate::automation::Target::Discovery(feed.feed_id),
        )
        .await
        .unwrap();
        assert!(!schedule.enabled);
        assert_eq!(schedule.version, 0);
        assert_eq!(
            discovery::feed(&pool, 1, feed.feed_id)
                .await
                .unwrap()
                .version,
            1
        );
        assert_eq!(
            downloads::job(&pool, 1, job.id).await.unwrap().status,
            "queued"
        );
        assert!(automation::due(&pool).await.unwrap().is_empty());
        pool.close().await;
    }

    /// Curated metadata, favorites and queue positions survive reopening.
    #[tokio::test]
    async fn reopening_preserves_curated_metadata_and_personal_state() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let pool = connect(&path).await.unwrap();
        users::create(&pool, "listener", &crate::auth::digest("listener"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (id,title) VALUES (1,'Original')")
            .execute(&pool)
            .await
            .unwrap();
        let kind = crate::library::enrichment::EntityKind::Recording;
        metadata::correct(
            &pool,
            kind,
            1,
            crate::library::enrichment::Correction {
                version: 1,
                name: "Curated".into(),
                artist_ids: vec![],
                release_kind: None,
                mbid: None,
            },
            1,
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO recording_reactions \
            (user_id,recording_id,reaction,changed_at) \
            VALUES (1,1,'like',CURRENT_TIMESTAMP)",
        )
        .execute(&pool)
        .await
        .unwrap();
        queue::update(&pool, 1, 0, Some(&[1, 1]), Some(2), 123)
            .await
            .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let metadata = metadata::get(&pool, kind, 1).await.unwrap();
        assert_eq!(metadata.name, "Curated");
        assert_eq!(metadata.version, 2);
        assert_eq!(
            reactions::favorites(&pool, 1, 50, 0).await.unwrap().len(),
            1
        );
        assert_eq!(
            queue::get(&pool, 1).await.unwrap().current_position,
            Some(2)
        );
        assert!(discovery::feeds(&pool, 1, 50, 0).await.unwrap().is_empty());
        assert_eq!(
            taste::profile(&pool, 1).await.unwrap().preferences.version,
            0
        );
        pool.close().await;
    }

    /// Scanner identities and personal queue snapshots survive reopening.
    #[tokio::test]
    async fn scanner_identity_and_queue_survive_reopening() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let pool = connect(&path).await.unwrap();
        users::create(&pool, "listener", &crate::auth::digest("listener"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO recordings (id,title) VALUES (1,'Curated')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scan_recording_matches (mbid,recording_id) \
            VALUES (?,1)",
        )
        .bind("12345678-1234-1234-1234-123456789abc")
        .execute(&pool)
        .await
        .unwrap();
        queue::update(&pool, 1, 0, Some(&[1, 1]), Some(2), 1234)
            .await
            .unwrap();
        pool.close().await;
        let pool = connect(&path).await.unwrap();
        let metadata = metadata::get(
            &pool,
            crate::library::enrichment::EntityKind::Recording,
            1,
        )
        .await
        .unwrap();
        assert_eq!(metadata.version, 1);
        assert_eq!(metadata.name, "Curated");
        assert_eq!(
            metadata.mbid.as_deref(),
            Some("12345678-1234-1234-1234-123456789abc")
        );
        let queue = queue::get(&pool, 1).await.unwrap();
        assert_eq!(queue.version, 1);
        assert_eq!(queue.current_position, Some(2));
        assert_eq!(queue.position_ms, 1234);
        assert!(metadata::jobs(&pool, 50, 0).await.unwrap().is_empty());
        pool.close().await;
    }

    /// Accounts default to members; explicit roles and keys survive reopening.
    #[tokio::test]
    async fn accounts_default_to_members_and_roles_survive_reopening() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("catalog.db");
        let pool = connect(&path).await.unwrap();
        sqlx::query("INSERT INTO users (id, name) VALUES (1, 'admin')")
            .execute(&pool)
            .await
            .unwrap();
        let digest = crate::auth::digest("personal-key");
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

    /// An empty SQLite database receives exactly one initial migration.
    #[tokio::test]
    async fn initializes_empty_database_with_one_migration() {
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
        assert_eq!(versions, [1]);
        for query in [
            "SELECT count(*) FROM client_artists",
            "SELECT count(*) FROM client_releases",
            "SELECT count(*) FROM client_recordings",
        ] {
            let count: i64 =
                sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
            assert_eq!(count, 0, "initial view must be empty: {query}");
        }
        let violations: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pragma_foreign_key_check",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(violations, 0);
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
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
