//! Entry point for the Shprotify music server.

mod api;
mod app;
mod cli;
mod config;
mod db;
mod jobs;
mod library;
mod logging;

/// Delegate startup and report any error through the process exit status.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
