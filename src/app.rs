//! Application startup orchestration.

use clap::Parser;

use crate::{cli::Cli, config::Config};

/// Parse startup arguments and validate configuration.
pub(crate) fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    Config::load(cli.config.as_deref())?;
    println!("Configuration is valid.");
    Ok(())
}
