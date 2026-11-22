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
    /// Set/reset a password and revoke sessions; personal keys remain valid.
    Password {
        /// Existing account name.
        #[arg(long)]
        name: String,
        /// Read one password from stdin instead of prompting with no echo.
        #[arg(long)]
        password_stdin: bool,
    },
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
        | UserCommand::Password { name, .. }
        | UserCommand::Role { name, .. } => auth::name(name)?,
    };
    let pool = db::connect(path).await?;
    let result = async {
        let output = match command {
            UserCommand::Password { password_stdin, .. } => {
                let from_stdin = *password_stdin;
                let password = tokio::task::spawn_blocking(move || {
                    read_password(from_stdin)
                })
                .await??;
                let hash = auth::password::hash(password).await?;
                let user =
                    db::sessions::set_password(&pool, &name, &hash).await?;
                serde_json::json!({"user": user, "password_set": true})
            }
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

/// Read a secret without command-line arguments, environment or echoed input.
fn read_password(
    from_stdin: bool,
) -> anyhow::Result<zeroize::Zeroizing<String>> {
    use std::io::IsTerminal;
    let password = if from_stdin {
        password_from_reader(std::io::stdin().lock())?
    } else {
        anyhow::ensure!(
            std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
            "Interactive password input requires a terminal; \
            use --password-stdin for redirected input"
        );
        let term = console::Term::stderr();
        term.write_str("Password: ")?;
        let password = zeroize::Zeroizing::new(term.read_secure_line()?);
        term.write_str("Confirm password: ")?;
        let confirmation = zeroize::Zeroizing::new(term.read_secure_line()?);
        anyhow::ensure!(*password == *confirmation, "Passwords do not match");
        password
    };
    crate::auth::password::validate(&password)?;
    Ok(password)
}

/// Read bounded UTF-8 through EOF, stripping only a single final LF or CRLF.
fn password_from_reader(
    reader: impl std::io::Read,
) -> anyhow::Result<zeroize::Zeroizing<String>> {
    use std::io::Read;
    let mut input = zeroize::Zeroizing::new(String::new());
    reader
        .take((crate::auth::password::MAX_BYTES + 3) as u64)
        .read_to_string(&mut input)?;
    if input.ends_with('\n') {
        input.pop();
        if input.ends_with('\r') {
            input.pop();
        }
    }
    crate::auth::password::validate(&input)?;
    Ok(input)
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

    /// Passwords come from a hidden prompt or bounded stdin, never argv.
    #[test]
    fn password_input_preserves_bytes_and_rejects_extra_lines() {
        let cli = Cli::try_parse_from([
            "shprotify",
            "user",
            "password",
            "--name",
            "alice",
            "--password-stdin",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::User {
                command: UserCommand::Password {
                    password_stdin: true,
                    ..
                }
            })
        ));
        assert!(
            Cli::try_parse_from([
                "shprotify",
                "user",
                "password",
                "--name",
                "alice",
                "--password",
                "a strong password",
            ])
            .is_err()
        );
        for suffix in ["", "\n", "\r\n"] {
            let input = format!("  a strong password  {suffix}");
            assert_eq!(
                &*password_from_reader(input.as_bytes()).unwrap(),
                "  a strong password  "
            );
        }
        for input in ["short", "a strong password\nextra", &"x".repeat(1027)] {
            assert!(password_from_reader(input.as_bytes()).is_err());
        }
        assert!(password_from_reader(&[0xff_u8][..]).is_err());
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
