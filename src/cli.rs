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
    /// Manage local accounts and revocable personal bearer keys.
    User {
        /// Operation; generated credentials are printed once to stdout.
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Read tags/properties as JSON Lines without changing the catalog.
    Scan,
}

/// Account operations require local access to the configured SQLite database.
#[derive(Debug, Subcommand)]
pub(crate) enum UserCommand {
    /// Set account permissions locally; existing keys use the new role.
    Role {
        /// Existing account name.
        #[arg(long)]
        name: String,
        /// Required privilege level: member or admin.
        #[arg(long, value_enum)]
        role: crate::auth::Role,
    },
    /// Create an account and print its first bearer key once as JSON.
    Create {
        /// Unique ASCII account name, normalized to lowercase.
        #[arg(long)]
        name: String,
    },
    /// Issue an additional bearer key for an existing account.
    Token {
        /// Existing account name.
        #[arg(long)]
        name: String,
    },
    /// Revoke every bearer key for an account without deleting personal data.
    Revoke {
        /// Existing account name.
        #[arg(long)]
        name: String,
    },
}

/// Execute local account management and close SQLite even on operation errors.
pub(crate) async fn run_user(
    path: &std::path::Path,
    command: &UserCommand,
) -> anyhow::Result<()> {
    use crate::{auth, db};
    let name = match command {
        UserCommand::Create { name }
        | UserCommand::Token { name }
        | UserCommand::Revoke { name }
        | UserCommand::Role { name, .. } => auth::name(name)?,
    };
    let pool = db::connect(path).await?;
    let result = async {
        let output = match command {
            UserCommand::Role { role, .. } => {
                let user = db::users::set_role(&pool, &name, *role).await?;
                serde_json::json!({"user": user})
            }
            UserCommand::Create { .. } | UserCommand::Token { .. } => {
                let token = auth::token()?;
                let digest = auth::digest(&token);
                let user = if matches!(command, UserCommand::Create { .. }) {
                    db::users::create(&pool, &name, &digest).await?
                } else {
                    db::users::issue(&pool, &name, &digest).await?
                };
                serde_json::json!({"user": user, "token": token})
            }
            UserCommand::Revoke { .. } => {
                let user = db::users::revoke(&pool, &name).await?;
                serde_json::json!({"user": user, "revoked": true})
            }
        };
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer(&mut stdout, &output)?;
        writeln!(stdout)?;
        Ok(())
    }
    .await;
    pool.close().await;
    result
}

#[cfg(test)]
mod tests {
    //! Startup argument parsing and standard CLI exit behavior.

    use super::*;
    use clap::error::ErrorKind;

    /// Account commands accept the shared config flag, never credential input.
    #[test]
    fn parses_local_user_management_commands() {
        for operation in ["create", "token", "revoke"] {
            let cli = Cli::try_parse_from([
                "shprotify",
                "user",
                operation,
                "--name",
                "alice",
                "--config",
                "dev/config.toml",
            ])
            .unwrap();
            assert!(matches!(cli.command, Some(Command::User { .. })));
            assert_eq!(cli.config, Some(PathBuf::from("dev/config.toml")));
        }
        assert!(Cli::try_parse_from(["shprotify", "user", "create"]).is_err());
    }

    /// Roles require an explicit valid choice; there is no implicit promotion.
    #[test]
    fn parses_explicit_account_role_changes() {
        let cli = Cli::try_parse_from([
            "shprotify",
            "user",
            "role",
            "--name",
            "alice",
            "--role",
            "admin",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::User {
                command: UserCommand::Role {
                    role: crate::auth::Role::Admin,
                    ..
                }
            })
        ));
        for args in [
            vec!["shprotify", "user", "role", "--name", "alice"],
            vec![
                "shprotify",
                "user",
                "role",
                "--name",
                "alice",
                "--role",
                "superuser",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

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
