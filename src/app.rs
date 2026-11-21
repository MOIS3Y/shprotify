//! Application startup orchestration.

use std::{future::Future, net::SocketAddr, path::Path};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::{
    net::TcpListener,
    signal::unix::{SignalKind, signal},
};

use crate::{api, cli::Cli, config::Config, db, logging};

/// Load settings, initialize logs, and serve HTTP until a shutdown signal.
///
/// Signal registration and binding errors stop startup. The server drains
/// active requests before returning after SIGINT or SIGTERM. Migrations run
/// before serving HTTP, and the pool is closed when HTTP serving finishes
/// or fails, including listener binding errors.
pub(crate) async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    logging::init(&config.logging)?;

    let shutdown = shutdown_signal()?;
    let database = db::connect(Path::new(&config.database.path)).await?;
    tracing::info!("SQLite database initialized; migrations applied");
    let result = serve_http(config.http.bind, shutdown).await;
    database.close().await;
    result
}

/// Bind the listener and serve requests until graceful shutdown completes.
async fn serve_http(
    address: SocketAddr,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let listener = TcpListener::bind(address).await.with_context(|| {
        format!("Failed to bind HTTP listener to {address}")
    })?;
    let bound_address = listener
        .local_addr()
        .context("Failed to read HTTP listener address")?;
    tracing::info!(address = %bound_address, "HTTP server listening");

    axum::serve(listener, api::router())
        .with_graceful_shutdown(shutdown)
        .await
        .context("HTTP server failed")?;
    tracing::info!("HTTP server stopped");
    Ok(())
}

/// Register Unix signal handlers before the HTTP listener starts accepting.
///
/// The returned future resolves on either SIGINT or SIGTERM and initiates
/// graceful shutdown. Registration failures are returned during startup.
fn shutdown_signal() -> Result<impl Future<Output = ()> + Send + 'static> {
    let mut interrupt = signal(SignalKind::interrupt())
        .context("Failed to register SIGINT handler")?;
    let mut terminate = signal(SignalKind::terminate())
        .context("Failed to register SIGTERM handler")?;

    Ok(async move {
        let signal = tokio::select! {
            _ = interrupt.recv() => "SIGINT",
            _ = terminate.recv() => "SIGTERM",
        };
        tracing::info!(signal, "Shutdown requested; draining active requests");
    })
}
