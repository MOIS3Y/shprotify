//! Global logging initialization from validated application settings.

use std::io::{self, IsTerminal};

use anyhow::{Context, Result};
use tracing::Level;

use crate::config::{LogFormat, LogLevel, LoggingConfig};

/// Install one global subscriber writing pretty or JSON events to stderr.
///
/// The configured level is authoritative; `RUST_LOG` is not consulted.
/// ANSI styling is enabled only for pretty output attached to a terminal.
/// Calling this more than once returns an initialization error.
pub(crate) fn init(config: &LoggingConfig) -> Result<()> {
    let level = match config.level {
        LogLevel::Trace => Level::TRACE,
        LogLevel::Debug => Level::DEBUG,
        LogLevel::Info => Level::INFO,
        LogLevel::Warn => Level::WARN,
        LogLevel::Error => Level::ERROR,
    };
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(io::stderr);

    let result = match config.format {
        LogFormat::Pretty => subscriber
            .with_ansi(io::stderr().is_terminal())
            .pretty()
            .try_init(),
        LogFormat::Json => subscriber.with_ansi(false).json().try_init(),
    };
    result
        .map_err(anyhow::Error::msg)
        .context("Failed to initialize logging")
}
