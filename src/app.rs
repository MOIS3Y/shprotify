//! Application startup orchestration.

use std::{
    future::Future,
    io::{self, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use tokio::{
    net::TcpListener,
    signal::unix::{SignalKind, signal},
};

use crate::{
    api,
    cli::{Cli, Command},
    config::Config,
    db,
    jobs::ScanService,
    library::{directories, scan},
    logging,
};

/// Load settings and either scan the collection or serve HTTP.
///
/// Signal registration and binding errors stop startup. The server drains
/// active requests before returning after SIGINT or SIGTERM. Migrations run
/// before serving HTTP, and the pool is closed when HTTP serving finishes
/// or fails, including listener binding errors. External directory checks
/// complete before opening the database and run outside async worker threads.
/// Unfinished persisted scan jobs are marked interrupted before HTTP startup.
/// Accepted background scans finish before the database pool closes.
pub(crate) async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    logging::init(&config.logging)?;
    if let Some(Command::User { command }) = &cli.command {
        return crate::cli::run_user(
            Path::new(&config.database.path),
            command,
        )
        .await;
    }

    let paths = config.library.directories.clone();
    let roots =
        tokio::task::spawn_blocking(move || directories::prepare(&paths))
            .await
            .context("Failed to prepare library directories")??;
    tracing::info!(
        directory_count = roots.len(),
        "External library directories validated"
    );
    if matches!(cli.command, Some(Command::Scan)) {
        return scan_library(roots).await;
    }
    let shutdown = shutdown_signal()?;
    let database = db::connect(Path::new(&config.database.path)).await?;
    tracing::info!("SQLite database initialized; migrations applied");
    if let Err(error) = db::scans::recover(&database).await {
        database.close().await;
        return Err(error).context("Failed to recover library scan jobs");
    }
    if let Err(error) = db::metadata::recover(&database).await {
        database.close().await;
        return Err(error).context("Failed to recover metadata jobs");
    }
    let scans = ScanService::new(database.clone(), roots);
    let metadata = match crate::jobs::metadata::MetadataService::new(
        database.clone(),
        scans.roots(),
        config.metadata.clone(),
    ) {
        Ok(service) => service,
        Err(error) => {
            database.close().await;
            return Err(error)
                .context("Failed to initialize metadata adapters");
        }
    };
    if let Err(error) = db::discovery::recover(&database).await {
        database.close().await;
        return Err(error).context("Failed to recover discovery jobs");
    }
    let discovery = match crate::jobs::discovery::DiscoveryService::new(
        database.clone(),
        config.discovery.clone(),
    ) {
        Ok(service) => service,
        Err(error) => {
            database.close().await;
            return Err(error)
                .context("Failed to initialize discovery adapters");
        }
    };
    let storage = if config.downloads.directory.as_os_str().is_empty() {
        None
    } else {
        let path = config.downloads.directory.clone();
        let external = scans.roots();
        Some(
            tokio::task::spawn_blocking(move || {
                crate::downloads::storage::Storage::prepare(&path, &external)
            })
            .await
            .context("Managed storage worker failed")??,
        )
    };
    let removals = crate::jobs::removals::RemovalService::new(
        database.clone(),
        storage.clone(),
    );
    removals
        .recover()
        .await
        .context("Failed to recover file removals")?;
    let downloads = crate::jobs::downloads::DownloadService::new(
        database.clone(),
        config.downloads.clone(),
        storage,
    );
    if let Err(error) = downloads.recover().await {
        database.close().await;
        return Err(error).context("Failed to recover acquisition jobs");
    }
    let automation = crate::jobs::automation::AutomationService::new(
        database.clone(),
        config.automation.clone(),
        scans.clone(),
        discovery.clone(),
        metadata.clone(),
    )
    .start()
    .await;
    let automation = match automation {
        Ok(worker) => worker,
        Err(error) => {
            downloads.close().await;
            discovery.close().await;
            metadata.close().await;
            scans.close().await;
            database.close().await;
            return Err(error)
                .context("Failed to initialize automation schedules");
        }
    };
    let stop_automation = automation.stop_signal();
    let shutdown = async move {
        shutdown.await;
        stop_automation.send_replace(true);
    };
    let result = serve_http(
        &config,
        &database,
        api::Services {
            scans: scans.clone(),
            metadata: metadata.clone(),
            discovery: discovery.clone(),
            downloads: downloads.clone(),
            automation: config.automation.clone(),
            clients: config.client_settings(),
            removals: removals.clone(),
        },
        shutdown,
    )
    .await;
    automation.close().await;
    removals.close().await;
    downloads.close().await;
    discovery.close().await;
    metadata.close().await;
    scans.close().await;
    database.close().await;
    result
}

/// Stream scan results to stdout without opening SQLite or an HTTP listener.
///
/// File failures still produce a final summary, followed by a failing exit
/// status. Logging stays on stderr; a broken report sink stops traversal.
async fn scan_library(roots: Vec<PathBuf>) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut output = io::BufWriter::new(io::stdout().lock());
        let summary = scan::run(&roots, |event| {
            serde_json::to_writer(&mut output, &event)
                .context("Failed to serialize scan report")?;
            writeln!(output).context("Failed to write scan report")?;
            Ok(())
        })?;
        output.flush().context("Failed to flush scan report")?;
        ensure!(
            summary.errors == 0,
            "Scan finished with {} error(s); see the JSON Lines report",
            summary.errors
        );
        Ok(())
    })
    .await
    .context("Library scanner worker failed")?
}

/// Bind the listener and serve requests until graceful shutdown completes.
async fn serve_http(
    config: &Config,
    database: &sqlx::SqlitePool,
    services: api::Services,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let address = config.http.bind;
    let listener = TcpListener::bind(address).await.with_context(|| {
        format!("Failed to bind HTTP listener to {address}")
    })?;
    let bound_address = listener
        .local_addr()
        .context("Failed to read HTTP listener address")?;
    tracing::info!(address = %bound_address, "HTTP server listening");

    axum::serve(
        listener,
        api::router_with_services(
            &config.auth,
            &config.docs,
            database,
            services,
        ),
    )
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
