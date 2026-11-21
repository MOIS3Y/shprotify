//! Entry point for the Shprotify music server.

mod app;
mod cli;
mod config;

/// Delegate startup and report any error through the process exit status.
fn main() -> anyhow::Result<()> {
    app::run()
}
