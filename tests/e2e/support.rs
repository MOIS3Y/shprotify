//! Isolated executable fixtures; all interactions use public CLI and HTTP.

use std::{
    fs,
    net::SocketAddr,
    path::PathBuf,
    process::{Child, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde_json::Value;
use tempfile::TempDir;
use tokio::{io::AsyncWriteExt, process::Command, time::Instant};

use super::media;
pub use super::media::{cover, wave};

/// Deadline for CLI commands, startup, shutdown and background jobs.
const DEADLINE: Duration = Duration::from_secs(30);

/// Private artifacts and a server killed/reaped even if a scenario panics.
pub struct Fixture {
    /// The first account is explicitly promoted through the local CLI.
    pub admin: String,
    /// Retain fixture files until the server has stopped.
    directory: TempDir,
    /// One isolated application process; absent before startup/after shutdown.
    child: Option<Child>,
    /// Actual loopback address read from the application's startup log.
    base: String,
    /// Bounded HTTP client that ignores ambient proxies and redirects.
    client: Client,
}

impl Fixture {
    /// Prepare a private collection and optionally a fake acquisition process.
    pub async fn new(downloads: bool) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir(root.join("music"))?;
        fs::write(root.join("music/song.wav"), wave())?;
        let provider = media::provider(root)?;
        let config = format!(
            r#"[http]
bind = "127.0.0.1:0"
[database]
path = {database}
[library]
directories = [{music}]
public_catalog = false
[logging]
format = "json"
[docs]
enabled = true
[auth]
session_ttl_seconds = 3600
[downloads]
enabled = {downloads}
directory = {managed}
request_interval_seconds = 1
[downloads.youtube]
enabled = {downloads}
yt_dlp_path = {provider}
ffmpeg_path = {provider}
deno_path = {provider}
"#,
            database = serde_json::to_string(&root.join("catalog.db"))?,
            music = serde_json::to_string(&root.join("music"))?,
            managed = serde_json::to_string(&root.join("managed"))?,
            provider = serde_json::to_string(&provider)?,
        );
        fs::write(root.join("config.toml"), config)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(5))
            .build()?;
        let mut fixture = Self {
            admin: String::new(),
            directory,
            child: None,
            base: String::new(),
            client,
        };
        let account = fixture
            .cli(&["user", "create", "--name", "operator"], None)
            .await?;
        ensure!(account["user"]["role"] == "member");
        fixture.admin = string(&account, "token")?.to_owned();
        fixture
            .cli(
                &["user", "role", "--name", "operator", "--role", "admin"],
                None,
            )
            .await?;
        Ok(fixture)
    }

    /// Resolve a fixture artifact, never a user's configuration or database.
    pub fn path(&self, relative: &str) -> PathBuf {
        self.directory.path().join(relative)
    }

    /// Clear ambient application settings while retaining pinned tool paths.
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_shprotify"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.directory.path())
            .env("TOKIO_WORKER_THREADS", "2")
            .current_dir(self.directory.path())
            .args(["--config", "config.toml"])
            .kill_on_drop(true);
        if let Some(certificates) = std::env::var_os("SSL_CERT_FILE") {
            command.env("SSL_CERT_FILE", certificates);
        }
        command
    }

    /// Bound CLI execution; passwords travel through stdin, not argv or env.
    pub async fn cli(
        &self,
        arguments: &[&str],
        input: Option<&str>,
    ) -> Result<Value> {
        let mut command = self.command();
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        let operation = async {
            if let Some(mut stdin) = child.stdin.take()
                && let Some(input) = input
            {
                stdin.write_all(input.as_bytes()).await?;
            }
            let output = child.wait_with_output().await?;
            ensure!(
                output.status.success(),
                "CLI failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice(&output.stdout).context("Invalid CLI JSON")
        };
        tokio::time::timeout(DEADLINE, operation)
            .await
            .context("CLI timed out")?
    }

    /// Provision a member via the same CLI an operator would use.
    pub async fn member(&self, name: &str) -> Result<String> {
        let account =
            self.cli(&["user", "create", "--name", name], None).await?;
        Ok(string(&account, "token")?.to_owned())
    }

    /// Start the real server on port zero and discover its assigned address.
    pub async fn start(&mut self) -> Result<()> {
        ensure!(self.child.is_none(), "Server already running");
        let log = fs::File::create(self.path("server.log"))?;
        let mut command = self.command().into_std();
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        self.child = Some(command.spawn()?);
        let deadline = Instant::now() + DEADLINE;
        loop {
            let log = fs::read_to_string(self.path("server.log"))?;
            if let Some(address) = listening_address(&log)? {
                self.base = format!("http://{address}");
                let response = self
                    .request(Method::GET, "/api/v1/health", "")
                    .send()
                    .await?;
                ensure!(response.status() == StatusCode::OK);
                return Ok(());
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait()? {
                bail!("Server exited during startup ({status}): {log}");
            }
            ensure!(Instant::now() < deadline, "Server startup timed out");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Gracefully stop the application and wait for a successful exit.
    pub async fn stop(&mut self) -> Result<()> {
        let child = self.child.as_mut().context("Server not running")?;
        let pid = i32::try_from(child.id())?;
        let pid =
            rustix::process::Pid::from_raw(pid).context("Invalid PID")?;
        rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = child.try_wait()? {
                self.child = None;
                self.base.clear();
                ensure!(status.success(), "Server shutdown failed: {status}");
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "Server shutdown timed out");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Build a request confined to this fixture's loopback server.
    pub fn request(
        &self,
        method: Method,
        path: &str,
        token: &str,
    ) -> RequestBuilder {
        assert!(!self.base.is_empty() && path.starts_with('/'));
        let request =
            self.client.request(method, format!("{}{path}", self.base));
        if token.is_empty() {
            request
        } else {
            request.bearer_auth(token)
        }
    }

    /// Check status without printing response bodies containing credentials.
    pub async fn response(
        &self,
        request: RequestBuilder,
        expected: StatusCode,
    ) -> Result<Response> {
        let response = request.send().await?;
        if response.status() != expected {
            let log = fs::read_to_string(self.path("server.log"))?;
            bail!(
                "Expected HTTP {expected}, got {} at {}:\n{log}",
                response.status(),
                response.url().path()
            );
        }
        Ok(response)
    }

    /// Send JSON and decode a response; 204 responses must have no body.
    pub async fn json(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
        expected: StatusCode,
    ) -> Result<Value> {
        let mut request = self.request(method, path, token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = self.response(request, expected).await?;
        if expected == StatusCode::NO_CONTENT {
            ensure!(response.bytes().await?.is_empty());
            Ok(Value::Null)
        } else {
            response.json().await.context("Invalid API JSON")
        }
    }

    /// Read a public JSON resource using the chosen account's credentials.
    pub async fn get(&self, path: &str, token: &str) -> Result<Value> {
        self.json(Method::GET, path, token, None, StatusCode::OK)
            .await
    }

    /// Poll a durable job to a terminal status within a deadline.
    pub async fn wait_job(&self, path: &str, token: &str) -> Result<Value> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let job = self
                .json(Method::GET, path, token, None, StatusCode::OK)
                .await?;
            let status = string(&job, "status")?;
            if matches!(
                status,
                "completed" | "failed" | "interrupted" | "cancelled"
            ) {
                return Ok(job);
            }
            ensure!(Instant::now() < deadline, "Job timed out: {path}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Index the generated collection through the public administrative API.
    pub async fn scan(&self) -> Result<Value> {
        let job = self
            .json(
                Method::POST,
                "/api/v1/library/scans",
                &self.admin,
                None,
                StatusCode::ACCEPTED,
            )
            .await?;
        self.wait_job(
            &format!("/api/v1/library/scans/{}", id(&job, "id")?),
            &self.admin,
        )
        .await
    }

    /// Prepare one indexed recording as setup for personal workflow tests.
    pub async fn indexed_recording(&self) -> Result<Value> {
        let scan = self.scan().await?;
        ensure!(scan["status"] == "completed" && scan["errors"] == 0);
        let recordings = self.get("/api/v1/recordings", &self.admin).await?;
        let items =
            recordings["items"].as_array().context("Missing records")?;
        ensure!(items.len() == 1, "Expected one generated recording");
        Ok(items[0].clone())
    }
}

impl Drop for Fixture {
    /// Last-resort cleanup on failure; normal scenarios use graceful shutdown.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Parse complete JSON log lines, tolerating an incomplete final write.
fn listening_address(log: &str) -> Result<Option<SocketAddr>> {
    for line in log.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event["fields"]["message"] == "HTTP server listening" {
            let address: SocketAddr =
                string(&event["fields"], "address")?.parse()?;
            ensure!(address.ip().is_loopback() && address.port() != 0);
            return Ok(Some(address));
        }
    }
    Ok(None)
}

/// Extract an expected string without dumping potentially secret JSON.
pub fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .with_context(|| format!("Missing string field {field}"))
}

/// Extract a positive resource ID from a public response.
pub fn id(value: &Value, field: &str) -> Result<i64> {
    let id = value[field]
        .as_i64()
        .with_context(|| format!("Missing integer field {field}"))?;
    ensure!(id > 0, "Invalid resource ID");
    Ok(id)
}
