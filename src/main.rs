//! Entry point for the Shprotify music server.

mod api;
mod app;
mod audio;
mod auth;
mod automation;
mod cli;
mod config;
mod db;
mod discovery;
mod downloads;
mod jobs;
mod library;
mod logging;
mod playlists;
mod providers;

/// Delegate startup and report any error through the process exit status.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
