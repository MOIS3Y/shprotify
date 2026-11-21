//! Command-line arguments for application startup.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Startup options; configuration values come from TOML and environment.
#[derive(Debug, Parser)]
#[command(version, about)]
pub(crate) struct Cli {
    /// Load this TOML file before applying SHPROTIFY_* overrides.
    #[arg(short, long, global = true, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,
    /// Optional operation; omitting it starts the HTTP server.
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

/// Explicit operations that share the server configuration.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Read tags/properties as JSON Lines without changing the catalog.
    Scan,
}

#[cfg(test)]
mod tests {
    //! Startup argument parsing and standard CLI exit behavior.

    use super::*;
    use clap::error::ErrorKind;

    /// Startup works without an explicit configuration file.
    #[test]
    fn config_path_is_optional() {
        assert!(Cli::try_parse_from(["shprotify"]).unwrap().config.is_none());
    }

    /// The configuration option preserves the supplied path.
    #[test]
    fn accepts_explicit_config_path() {
        let cli = Cli::try_parse_from([
            "shprotify",
            "--config",
            "dev/shprotify.toml",
        ])
        .unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("dev/shprotify.toml")));
    }

    /// A configuration option without a value is a CLI error.
    #[test]
    fn config_requires_a_path() {
        let error =
            Cli::try_parse_from(["shprotify", "--config"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    /// Scan accepts the shared configuration option in either position.
    #[test]
    fn accepts_scan_with_global_config() {
        for args in [
            ["shprotify", "scan", "--config", "music.toml"],
            ["shprotify", "--config", "music.toml", "scan"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.command, Some(Command::Scan)));
            assert_eq!(cli.config, Some(PathBuf::from("music.toml")));
        }
    }

    /// Help and version request successful early exits from Clap.
    #[test]
    fn help_and_version_exit_successfully() {
        for (flag, kind) in [
            ("--help", ErrorKind::DisplayHelp),
            ("--version", ErrorKind::DisplayVersion),
        ] {
            let error = Cli::try_parse_from(["shprotify", flag]).unwrap_err();
            assert_eq!(error.kind(), kind);
        }
    }
}
