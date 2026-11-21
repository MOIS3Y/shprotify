//! Typed startup configuration.
//!
//! Precedence: defaults, explicit TOML file, then `SHPROTIFY_*` variables.
//! Separate nested keys with `__`, for example `SHPROTIFY_HTTP__BIND`.
//! Relative database paths are resolved against the working directory.

use std::{fs, net::SocketAddr, path::Path};

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
    /// HTTP listener settings.
    pub(crate) http: HttpConfig,
    /// SQLite storage settings.
    pub(crate) database: DatabaseConfig,
    /// Log filtering and output settings.
    pub(crate) logging: LoggingConfig,
    /// Optional interactive API documentation and OpenAPI schema.
    pub(crate) docs: DocsConfig,
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

    /// Deserialize merged values and reject a blank database path.
    ///
    /// Serde rejects unknown fields, invalid enum variants, and malformed
    /// socket addresses before semantic validation runs.
    fn extract(figment: Figment) -> Result<Self> {
        let config: Self =
            figment.extract().context("Invalid configuration")?;
        ensure!(
            !config.database.path.trim().is_empty(),
            "database.path must not be empty"
        );
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

    /// Defaults provide every setting when no file is supplied.
    #[test]
    fn loads_defaults_without_a_file() {
        let config = Config::extract(Config::sources(None).unwrap()).unwrap();
        assert_eq!(config.http.bind.to_string(), "127.0.0.1:3000");
        assert_eq!(config.database.path, "shprotify.db");
        assert!(matches!(config.logging.level, LogLevel::Info));
        assert!(matches!(config.logging.format, LogFormat::Pretty));
        assert!(!config.docs.enabled);
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
            "[http]\nbind = 'localhost:3000'",
            "[http]\nbind = '127.0.0.1:99999'",
            "[database]\npath = '  '",
            "[logging]\nlevel = 'verbose'",
            "[logging]\nformat = 'xml'",
            "[docs]\nenabled = 'yes'",
            "[docs]\nunknown = true",
            "[http]\nport = 3000",
            "[unknown]\nenabled = true",
            "[http",
        ] {
            assert!(from_toml(contents).is_err(), "accepted: {contents}");
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
}
