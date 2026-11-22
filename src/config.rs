//! Typed startup configuration.
//!
//! Precedence: defaults, explicit TOML file, then `SHPROTIFY_*` variables.
//! Separate nested keys with `__`, for example `SHPROTIFY_HTTP__BIND`.
//! Relative database paths are resolved against the working directory.

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

/// Validated application settings.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Experimental audio acquisition and managed storage.
    pub(crate) downloads: DownloadsConfig,
    /// Recommendation adapters, response caching and refresh admission.
    pub(crate) discovery: DiscoveryConfig,
    /// Optional external metadata services and acoustic identification.
    pub(crate) metadata: MetadataConfig,
    /// Password session settings, applied when issuing new credentials.
    pub(crate) auth: AuthConfig,
    /// External collection roots checked separately before HTTP startup.
    pub(crate) library: LibraryConfig,
    /// HTTP listener settings.
    pub(crate) http: HttpConfig,
    /// SQLite storage settings.
    pub(crate) database: DatabaseConfig,
    /// Log filtering and output settings.
    pub(crate) logging: LoggingConfig,
    /// Optional interactive API documentation and OpenAPI schema.
    pub(crate) docs: DocsConfig,
}

/// Bounded opt-in acquisition; members never supply executable arguments.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DownloadsConfig {
    /// Enable acquisition endpoints and managed storage initialization.
    pub(crate) enabled: bool,
    /// Exclusive managed root; must not overlap external library roots.
    pub(crate) directory: PathBuf,
    /// Concurrent provider processes, including searches; 1–4.
    pub(crate) max_parallel_jobs: usize,
    /// Total queued/active jobs across all users; 1–100.
    pub(crate) max_pending_jobs: i64,
    /// Queued/active jobs for one member or admin; 1–10.
    pub(crate) max_user_jobs: i64,
    /// Maximum encoded audio bytes per job; 1 MiB–1 GiB.
    pub(crate) max_file_bytes: u64,
    /// Download deadline including provider resolution; 30–1800 seconds.
    pub(crate) timeout_seconds: u64,
    /// Maximum accepted source/audio duration; 1–14400 seconds.
    pub(crate) max_duration_seconds: u32,
    /// Minimum interval between source searches per account; 1–300 seconds.
    pub(crate) request_interval_seconds: u32,
    /// Experimental YouTube adapter and executable paths.
    pub(crate) youtube: YoutubeConfig,
}

impl Default for DownloadsConfig {
    /// No process invocation or managed filesystem writes without opt-in.
    fn default() -> Self {
        Self {
            enabled: false,
            directory: PathBuf::new(),
            max_parallel_jobs: 2,
            max_pending_jobs: 20,
            max_user_jobs: 3,
            max_file_bytes: 256 * 1024 * 1024,
            timeout_seconds: 600,
            max_duration_seconds: 1800,
            request_interval_seconds: 5,
            youtube: YoutubeConfig::default(),
        }
    }
}

/// Server-owned tools; configuration never accepts arbitrary CLI flags.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct YoutubeConfig {
    /// Explicitly enable the unstable YouTube provider.
    pub(crate) enabled: bool,
    /// yt-dlp binary or wrapper, resolved through PATH unless absolute.
    pub(crate) yt_dlp_path: String,
    /// FFmpeg used only for container extraction, without lossy re-encoding.
    pub(crate) ffmpeg_path: String,
    /// Deno runtime for bundled yt-dlp EJS scripts.
    pub(crate) deno_path: String,
}

impl Default for YoutubeConfig {
    /// Tool names supplied by the Nix package or development shell.
    fn default() -> Self {
        Self {
            enabled: false,
            yt_dlp_path: "yt-dlp".into(),
            ffmpeg_path: "ffmpeg".into(),
            deno_path: "deno".into(),
        }
    }
}

impl DownloadsConfig {
    /// Reject resource limits and paths that cannot be safely represented.
    fn validate(&self) -> Result<()> {
        ensure!(
            !self.enabled || !self.directory.as_os_str().is_empty(),
            "downloads.directory is required when downloads are enabled"
        );
        ensure!(
            (1..=4).contains(&self.max_parallel_jobs)
                && (1..=100).contains(&self.max_pending_jobs)
                && (1..=10).contains(&self.max_user_jobs)
                && (1_048_576..=1_073_741_824).contains(&self.max_file_bytes)
                && (30..=1800).contains(&self.timeout_seconds)
                && (1..=14400).contains(&self.max_duration_seconds)
                && (1..=300).contains(&self.request_interval_seconds),
            "Invalid downloads resource limits"
        );
        for path in [
            &self.youtube.yt_dlp_path,
            &self.youtube.ffmpeg_path,
            &self.youtube.deno_path,
        ] {
            ensure!(
                !path.trim().is_empty() && !path.chars().any(char::is_control),
                "Download tool paths must be nonblank without controls"
            );
        }
        Ok(())
    }
}

/// Optional discovery providers; local recommendations need no external key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoveryConfig {
    /// Enable explicit external recommendation requests.
    pub(crate) enabled: bool,
    /// Meaningful application identity and operator contact.
    pub(crate) user_agent: String,
    /// Last.fm application key; blank leaves its adapter disabled.
    pub(crate) lastfm_api_key: String,
    /// Enable fetching public ListenBrainz playlists by ID.
    pub(crate) listenbrainz_enabled: bool,
    /// Enable recent-release metadata queries for exact artist MBID seeds.
    pub(crate) musicbrainz_enabled: bool,
    /// Normalized public response lifetime, 60–86400 seconds.
    pub(crate) cache_ttl_seconds: u32,
    /// Minimum interval between accepted refreshes for one account, 1–3600.
    pub(crate) refresh_interval_seconds: u32,
}

impl Default for DiscoveryConfig {
    /// Local-only discovery; no network requests occur automatically.
    fn default() -> Self {
        Self {
            enabled: false,
            user_agent: String::new(),
            lastfm_api_key: String::new(),
            listenbrainz_enabled: false,
            musicbrainz_enabled: false,
            cache_ttl_seconds: 3600,
            refresh_interval_seconds: 60,
        }
    }
}

/// Opt-in external lookups; no network requests occur during normal scans.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MetadataConfig {
    /// Allow MusicBrainz search/lookup and Cover Art Archive jobs.
    pub(crate) enabled: bool,
    /// Application identity with operator contact, required when enabled.
    pub(crate) user_agent: String,
    /// AcoustID application key; blank disables acoustic identification.
    pub(crate) acoustid_key: String,
    /// Chromaprint executable; empty disables fingerprint execution.
    pub(crate) fpcalc_path: String,
}

impl Default for MetadataConfig {
    /// Keep external calls and executable invocation disabled by default.
    fn default() -> Self {
        Self {
            enabled: false,
            user_agent: String::new(),
            acoustid_key: String::new(),
            fpcalc_path: String::new(),
        }
    }
}

/// Password sessions have an absolute lifetime independent of activity.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthConfig {
    /// Lifetime of newly issued sessions in seconds; must be positive.
    pub(crate) session_ttl_seconds: u32,
}

impl Default for AuthConfig {
    /// Allow clients to stay signed in for 30 days by default.
    fn default() -> Self {
        Self {
            session_ttl_seconds: 30 * 24 * 60 * 60,
        }
    }
}

/// External music directories; an empty list keeps catalog-only use possible.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LibraryConfig {
    /// Existing readable directories, relative to the working directory or
    /// absolute. Startup resolves aliases and rejects overlapping roots.
    pub(crate) directories: Vec<PathBuf>,
}

/// Documentation routes enabled at startup; disabled by default.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocsConfig {
    /// Publish Swagger UI and its OpenAPI JSON together after a restart.
    pub(crate) enabled: bool,
}

/// Address on which the HTTP server will listen.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HttpConfig {
    /// Numeric IPv4 or IPv6 socket address, including the port.
    pub(crate) bind: SocketAddr,
}

impl Default for HttpConfig {
    /// Listen on loopback port 3000 unless explicitly overridden.
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 3000)),
        }
    }
}

/// Location of the SQLite database file.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DatabaseConfig {
    /// Nonblank path, relative to the process working directory if needed.
    pub(crate) path: String,
}

impl Default for DatabaseConfig {
    /// Store the database in the process working directory by default.
    fn default() -> Self {
        Self {
            path: "shprotify.db".into(),
        }
    }
}

/// Filtering and rendering options for application logs.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoggingConfig {
    /// Minimum severity to emit.
    pub(crate) level: LogLevel,
    /// Output representation for each log event.
    pub(crate) format: LogFormat,
}

/// Supported minimum log severities, serialized in lowercase.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LogLevel {
    /// Include detailed execution traces.
    Trace,
    /// Include diagnostic events useful during development.
    Debug,
    /// Include normal operational events; the default severity.
    #[default]
    Info,
    /// Include warnings and errors only.
    Warn,
    /// Include errors only.
    Error,
}

/// Supported log representations, serialized in lowercase.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LogFormat {
    /// Human-readable console output; the default format.
    #[default]
    Pretty,
    /// Structured JSON events for automated log processing.
    Json,
}

impl Config {
    /// Load an optional explicit file and apply environment overrides.
    ///
    /// An explicit unreadable file is always an error. No file is searched
    /// for implicitly, and no directories or database files are created.
    pub(crate) fn load(path: Option<&Path>) -> Result<Self> {
        let figment = Self::sources(path)?
            .merge(Env::prefixed("SHPROTIFY_").split("__"));
        Self::extract(figment).with_context(|| match path {
            Some(path) => {
                format!("Failed to load configuration {}", path.display())
            }
            None => "Failed to load configuration".into(),
        })
    }

    /// Merge defaults with an optional file, leaving environment unapplied.
    ///
    /// Read the file explicitly so a missing or unreadable file cannot be
    /// silently replaced with defaults. TOML parsing errors are retained
    /// by Figment and reported during extraction.
    fn sources(path: Option<&Path>) -> Result<Figment> {
        let mut figment = Figment::from(Serialized::defaults(Self::default()));
        if let Some(path) = path {
            let contents = fs::read_to_string(path).with_context(|| {
                format!("Failed to read configuration {}", path.display())
            })?;
            figment = figment.merge(Toml::string(&contents));
        }
        Ok(figment)
    }

    /// Deserialize merged values and reject empty configured paths.
    ///
    /// Serde rejects unknown fields, invalid enum variants, and malformed
    /// socket addresses before semantic validation runs.
    fn extract(figment: Figment) -> Result<Self> {
        let config: Self =
            figment.extract().context("Invalid configuration")?;
        ensure!(
            config.auth.session_ttl_seconds > 0,
            "auth.session_ttl_seconds must be greater than zero"
        );
        ensure!(
            !config.database.path.trim().is_empty(),
            "database.path must not be empty"
        );
        ensure!(
            config
                .library
                .directories
                .iter()
                .all(|path| !path.as_os_str().is_empty()),
            "library.directories must not contain empty paths"
        );
        ensure!(
            !config.metadata.enabled
                || (config.metadata.user_agent.len() >= 10
                    && config.metadata.user_agent.len() <= 255
                    && config.metadata.user_agent.is_ascii()
                    && !config
                        .metadata
                        .user_agent
                        .chars()
                        .any(char::is_control)),
            "metadata.user_agent must identify the app and operator contact"
        );
        ensure!(
            config.metadata.acoustid_key.len() <= 255
                && !config.metadata.acoustid_key.chars().any(char::is_control),
            "Invalid metadata.acoustid_key"
        );
        ensure!(
            !config.discovery.enabled
                || (config.discovery.user_agent.len() >= 10
                    && config.discovery.user_agent.len() <= 255
                    && config.discovery.user_agent.is_ascii()
                    && !config
                        .discovery
                        .user_agent
                        .chars()
                        .any(char::is_control)),
            "discovery.user_agent must identify the app and operator contact"
        );
        ensure!(
            config.discovery.lastfm_api_key.len() <= 255
                && !config
                    .discovery
                    .lastfm_api_key
                    .chars()
                    .any(char::is_control),
            "Invalid discovery.lastfm_api_key"
        );
        ensure!(
            (60..=86400).contains(&config.discovery.cache_ttl_seconds)
                && (1..=3600)
                    .contains(&config.discovery.refresh_interval_seconds),
            "Invalid discovery cache TTL or refresh interval"
        );
        config.downloads.validate()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    //! Configuration loading, validation, and source precedence checks.

    use super::*;

    /// Load inline TOML over defaults without reading the process environment.
    fn from_toml(contents: &str) -> Result<Config> {
        Config::extract(Config::sources(None)?.merge(Toml::string(contents)))
    }

    /// Download defaults are inert; enabled acquisition requires bounded
    /// policy.
    #[test]
    fn validates_experimental_download_limits_and_tool_paths() {
        let config = from_toml("").unwrap();
        assert!(!config.downloads.enabled);
        assert!(!config.downloads.youtube.enabled);
        assert!(config.downloads.directory.as_os_str().is_empty());
        let enabled = from_toml(
            "[downloads]\nenabled=true\ndirectory='dev/downloads'\n\
             [downloads.youtube]\nenabled=true",
        )
        .unwrap();
        assert!(enabled.downloads.enabled);
        for input in [
            "enabled=true",
            "max_parallel_jobs=0",
            "max_parallel_jobs=5",
            "max_pending_jobs=101",
            "max_user_jobs=0",
            "max_user_jobs=11",
            "max_file_bytes=1048575",
            "max_file_bytes=1073741825",
            "timeout_seconds=29",
            "timeout_seconds=1801",
            "max_duration_seconds=0",
            "max_duration_seconds=14401",
            "request_interval_seconds=0",
            "request_interval_seconds=301",
            "unknown=true",
            "youtube={yt_dlp_path=''}",
            "youtube={extra_flags='--exec shell'}",
        ] {
            assert!(
                from_toml(&format!("[downloads]\n{input}")).is_err(),
                "accepted {input}"
            );
        }
    }

    /// Defaults provide every setting when no file is supplied.
    #[test]
    fn loads_defaults_without_a_file() {
        let config = Config::extract(Config::sources(None).unwrap()).unwrap();
        assert_eq!(config.http.bind.to_string(), "127.0.0.1:3000");
        assert_eq!(config.database.path, "shprotify.db");
        assert_eq!(config.auth.session_ttl_seconds, 2_592_000);
        assert!(matches!(config.logging.level, LogLevel::Info));
        assert!(matches!(config.logging.format, LogFormat::Pretty));
        assert!(!config.docs.enabled);
        assert!(config.library.directories.is_empty());
        assert!(!config.discovery.enabled);
        assert_eq!(config.discovery.cache_ttl_seconds, 3600);
        assert_eq!(config.discovery.refresh_interval_seconds, 60);
        assert!(!config.metadata.enabled);
        assert!(config.metadata.acoustid_key.is_empty());
    }

    /// Partial TOML preserves defaults for unspecified fields.
    #[test]
    fn partial_file_overrides_defaults() {
        let config = from_toml("[http]\nbind = '0.0.0.0:8080'").unwrap();
        assert_eq!(config.http.bind.to_string(), "0.0.0.0:8080");
        assert_eq!(config.database.path, "shprotify.db");
    }

    /// Later providers take precedence over earlier file values.
    #[test]
    fn later_source_overrides_file() {
        let figment = Config::sources(None)
            .unwrap()
            .merge(Toml::string("[http]\nbind = '127.0.0.1:4000'"))
            .merge(Toml::string("[http]\nbind = '127.0.0.1:5000'"));
        let config = Config::extract(figment).unwrap();
        assert_eq!(config.http.bind.port(), 5000);
    }

    /// Invalid types, values, syntax, and misspelled keys fail loading.
    #[test]
    fn rejects_invalid_values_and_unknown_fields() {
        for contents in [
            "[discovery]\nenabled = true",
            "[discovery]\ncache_ttl_seconds = 0",
            "[discovery]\ncache_ttl_seconds = 86401",
            "[discovery]\nrefresh_interval_seconds = 0",
            "[discovery]\nunknown = true",
            "[metadata]\nenabled = true",
            "[metadata]\nenabled = true\nuser_agent = 'bad'",
            "[metadata]\nunknown = true",
            "[auth]\nsession_ttl_seconds = 0",
            "[auth]\nsession_ttl_seconds = -1",
            "[auth]\nsession_ttl_seconds = 4294967296",
            "[auth]\nsession_ttl_seconds = 1.5",
            "[auth]\nsession_ttl_seconds = '30d'",
            "[auth]\nunknown = true",
            "[http]\nbind = 'localhost:3000'",
            "[http]\nbind = '127.0.0.1:99999'",
            "[database]\npath = '  '",
            "[logging]\nlevel = 'verbose'",
            "[logging]\nformat = 'xml'",
            "[docs]\nenabled = 'yes'",
            "[docs]\nunknown = true",
            "[library]\ndirectories = '/music'",
            "[library]\ndirectories = [42]",
            "[library]\ndirectories = ['']",
            "[library]\nunknown = true",
            "[http]\nport = 3000",
            "[unknown]\nenabled = true",
            "[http",
        ] {
            assert!(from_toml(contents).is_err(), "accepted: {contents}");
        }
    }

    /// Positive lifetimes, including the integer bounds, are configurable.
    #[test]
    fn session_lifetime_uses_seconds_and_preserves_other_defaults() {
        for seconds in [1, 3600, u32::MAX] {
            let config =
                from_toml(&format!("[auth]\nsession_ttl_seconds = {seconds}"))
                    .unwrap();
            assert_eq!(config.auth.session_ttl_seconds, seconds);
            assert_eq!(config.database.path, "shprotify.db");
        }
    }

    /// An unreadable explicit path is reported in the error.
    #[test]
    fn explicit_missing_file_is_an_error() {
        let path = Path::new("src/config.rs/missing.toml");
        let error = Config::sources(Some(path)).unwrap_err();
        assert!(error.to_string().contains(&path.display().to_string()));
    }

    /// The complete example remains compatible with the schema.
    #[test]
    fn example_file_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/shprotify.toml");
        let config =
            Config::extract(Config::sources(Some(&path)).unwrap()).unwrap();
        assert_eq!(config.database.path, "dev/shprotify.db");
    }

    /// Real environment overrides apply in an isolated child process.
    #[test]
    fn environment_overrides_file() {
        const CHILD: &str = "_SHPROTIFY_CONFIG_TEST_CHILD";
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/shprotify.toml");

        if std::env::var_os(CHILD).is_some() {
            let config = Config::load(Some(&path)).unwrap();
            assert_eq!(config.http.bind.port(), 9000);
            assert_eq!(config.database.path, "music files/test.db");
            assert!(matches!(config.logging.level, LogLevel::Debug));
            assert!(matches!(config.logging.format, LogFormat::Json));
            assert!(config.docs.enabled);
            assert_eq!(config.auth.session_ttl_seconds, 3600);
            assert_eq!(
                config.library.directories,
                [PathBuf::from("/music files"), PathBuf::from("/archive")]
            );
            return;
        }

        // Isolate environment changes from parallel tests without set_var.
        let mut command =
            std::process::Command::new(std::env::current_exe().unwrap());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("SHPROTIFY_") {
                command.env_remove(key);
            }
        }
        let output = command
            .args(["--exact", "config::tests::environment_overrides_file"])
            .env(CHILD, "1")
            .env("SHPROTIFY_HTTP__BIND", "127.0.0.1:9000")
            .env("SHPROTIFY_DATABASE__PATH", "music files/test.db")
            .env("SHPROTIFY_LOGGING__LEVEL", "debug")
            .env("SHPROTIFY_LOGGING__FORMAT", "json")
            .env("SHPROTIFY_DOCS__ENABLED", "true")
            .env("SHPROTIFY_AUTH__SESSION_TTL_SECONDS", "3600")
            .env(
                "SHPROTIFY_LIBRARY__DIRECTORIES",
                r#"["/music files", "/archive"]"#,
            )
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    /// Documentation can be enabled explicitly in TOML.
    #[test]
    fn documentation_can_be_enabled() {
        let config = from_toml("[docs]\nenabled = true").unwrap();
        assert!(config.docs.enabled);
    }

    /// Configuration parsing preserves paths; filesystem checks occur later.
    #[test]
    fn loads_external_directory_paths_without_accessing_them() {
        let config = from_toml(
            "[library]\ndirectories = ['dev/music', '/mnt/music archive']",
        )
        .unwrap();
        assert_eq!(
            config.library.directories,
            [
                PathBuf::from("dev/music"),
                PathBuf::from("/mnt/music archive")
            ]
        );
    }
}
