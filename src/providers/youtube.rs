//! Experimental YouTube adapter using a separately updatable yt-dlp process.
//!
//! No user URLs, shell commands, ambient configuration, cookies or plugins.
//! Provider output is bounded and never persisted as diagnostic text. Process
//! groups are killed on cancellation, timeout, parse failure and shutdown,
//! including FFmpeg/JavaScript descendants. This is not an OS sandbox.

use crate::{
    config::DownloadsConfig,
    downloads::{DownloadError, Result, Source, Target, text, video_id},
};
use serde_json::Value;
use std::{path::Path, process::Stdio};
use tokio::{io::AsyncReadExt, process::Command};

/// Kill the isolated Unix process group even when the awaiting future drops.
struct ProcessGroup(rustix::process::Pid);

impl Drop for ProcessGroup {
    /// SIGKILL is idempotent when the process group has already exited.
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(
            self.0,
            rustix::process::Signal::KILL,
        );
    }
}

/// Resolve server tool paths before changing into the job scratch directory.
/// FFmpeg's location option needs a filesystem path, not just a PATH command.
fn tool(value: &str) -> Result<std::path::PathBuf> {
    let path = std::path::Path::new(value);
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    if path.components().count() > 1 {
        return std::path::absolute(path).map_err(DownloadError::from);
    }
    Ok(std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(value))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| path.to_owned()))
}

/// Create only trusted, fixed flags; requests become a final single argument.
fn command(config: &DownloadsConfig) -> Result<Command> {
    let mut command = Command::new(tool(&config.youtube.yt_dlp_path)?);
    let path = std::env::var_os("PATH").unwrap_or_default();
    command
        .env_clear()
        .env("PATH", path)
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .process_group(0)
        .args([
            "--ignore-config",
            "--no-plugin-dirs",
            "--no-cache-dir",
            "--no-remote-components",
            "--no-js-runtimes",
            "--js-runtimes",
            &format!("deno:{}", tool(&config.youtube.deno_path)?.display()),
            "--no-playlist",
            "--no-warnings",
            "--no-colors",
            "--socket-timeout",
            "10",
            "--retries",
            "2",
            "--fragment-retries",
            "2",
            "--extractor-retries",
            "1",
            "--use-extractors",
            "youtube.*",
        ]);
    Ok(command)
}

/// Start a child whose descendants cannot outlive the owning provider future.
fn spawn(
    command: &mut Command,
) -> Result<(tokio::process::Child, ProcessGroup)> {
    let mut child = command.spawn().map_err(|_| {
        DownloadError::Failed("Cannot start yt-dlp; check server tool paths")
    })?;
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw);
    let Some(pid) = pid else {
        let _ = child.start_kill();
        return Err(DownloadError::Failed("Cannot supervise yt-dlp"));
    };
    Ok((child, ProcessGroup(pid)))
}

/// Resolve at most five results; flat search avoids resolving every CDN URL.
/// The worker applies a separate 60-second search deadline.
pub(crate) async fn search(
    config: &DownloadsConfig,
    target: &Target,
) -> Result<Vec<Source>> {
    let query = format!("{} {}", target.artists.join(" "), target.title);
    let mut command = command(config)?;
    command.args([
        "--flat-playlist",
        "--dump-single-json",
        "--",
        &format!("ytsearch5:{}", query.trim()),
    ]);
    command.stdout(Stdio::piped());
    let (mut child, _group) = spawn(&mut command)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(DownloadError::Failed("Cannot read yt-dlp search result"))?;
    let read = async move {
        let mut bytes = Vec::new();
        stdout
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(DownloadError::Failed(
                "Provider response is too large",
            ));
        }
        Ok::<_, DownloadError>(bytes)
    };
    let (status, bytes) = tokio::try_join!(
        async { child.wait().await.map_err(DownloadError::from) },
        read
    )?;
    if !status.success() {
        return Err(DownloadError::Failed(
            "YouTube search failed; update yt-dlp or try again later",
        ));
    }
    parse_search(&bytes, config.max_duration_seconds)
}

/// Validate provider metadata and retain stable video identity, not stream
/// URLs.
fn parse_search(bytes: &[u8], max_duration: u32) -> Result<Vec<Source>> {
    let data: Value = serde_json::from_slice(bytes).map_err(|_| {
        DownloadError::Failed("YouTube returned invalid search metadata")
    })?;
    let entries = data
        .get("entries")
        .and_then(Value::as_array)
        .filter(|e| e.len() <= 5)
        .ok_or(DownloadError::Failed("YouTube search format changed"))?;
    let mut results = Vec::new();
    for entry in entries {
        if entry.is_null() {
            continue;
        }
        let id = entry["id"]
            .as_str()
            .filter(|id| video_id(id))
            .ok_or(DownloadError::Failed("Invalid YouTube video identity"))?;
        let title = entry["title"]
            .as_str()
            .filter(|v| text(v).is_ok())
            .ok_or(DownloadError::Failed("Invalid YouTube display title"))?;
        if entry["is_live"].as_bool() == Some(true)
            || matches!(
                entry["live_status"].as_str(),
                Some("is_live" | "is_upcoming" | "post_live")
            )
        {
            continue;
        }
        let duration = entry["duration"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0)
            .map(|n| n.ceil() as i64);
        if duration.is_some_and(|d| d > i64::from(max_duration)) {
            continue;
        }
        if results.iter().any(|s: &Source| s.video_id == id) {
            continue;
        }
        let channel = entry["channel"]
            .as_str()
            .or_else(|| entry["uploader"].as_str())
            .filter(|s| text(s).is_ok())
            .map(str::to_owned);
        results.push(Source {
            id: 0,
            job_id: 0,
            video_id: id.into(),
            title: title.trim().into(),
            url: format!("https://www.youtube.com/watch?v={id}"),
            channel,
            duration_seconds: duration,
        });
    }
    Ok(results)
}

/// Download one audio-only stream, preserving its lossy codec without
/// recoding.
/// FFmpeg may remux Opus from WebM into an Ogg container understood by Lofty.
pub(crate) async fn download(
    config: &DownloadsConfig,
    id: &str,
    stage: &Path,
) -> Result<()> {
    if !video_id(id) {
        return Err(DownloadError::Invalid("Invalid YouTube video ID"));
    }
    let mut command = command(config)?;
    command.current_dir(stage).stdout(Stdio::null()).args([
        "--quiet",
        "--no-progress",
        "--no-simulate",
        "--abort-on-error",
        "--format",
        "bestaudio[acodec~='^(opus|mp4a)']",
        "--extract-audio",
        "--audio-format",
        "best",
        "--ffmpeg-location",
        &config.youtube.ffmpeg_path,
        "--max-filesize",
        &config.max_file_bytes.to_string(),
        "--match-filters",
        &format!(
            "duration <= {} & !is_live & live_status !=? is_upcoming \
                    & live_status !=? post_live",
            config.max_duration_seconds
        ),
        "--concurrent-fragments",
        "1",
        "--no-keep-video",
        "--no-write-info-json",
        "--no-write-thumbnail",
        "--no-write-subs",
        "--output",
        "audio.%(ext)s",
        "--",
        &format!("https://www.youtube.com/watch?v={id}"),
    ]);
    let (mut child, _group) = spawn(&mut command)?;
    if !child.wait().await?.success() {
        return Err(DownloadError::Failed(
            "YouTube download failed; source unavailable or yt-dlp outdated",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Stable provider output contract and strict video-only input boundaries.
    use super::*;

    /// Partial results are bounded, deduplicated and filtered before display.
    #[test]
    fn parses_only_bounded_non_live_video_sources() {
        let data = serde_json::json!({"entries": [
            {"id":"abcdefghijk", "title":" Кино — Трек ",
             "channel":"Кино", "duration":222.2},
            {"id":"abcdefghijk", "title":"Duplicate"},
            {"id":"01234567890", "title":"Live", "is_live":true},
            {"id":"ABCDEFGHIJK", "title":"Long", "duration":9999}, null
        ]});
        let results =
            parse_search(&serde_json::to_vec(&data).unwrap(), 1800).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].duration_seconds, Some(223));
        assert_eq!(results[0].title, "Кино — Трек");
        for data in [
            b"{}".as_slice(),
            b"not json",
            br#"{"entries":[{"id":"https://evil","title":"X"}]}"#,
        ] {
            assert!(parse_search(data, 1800).is_err());
        }
        for id in
            ["", "--exec=cmd", "../../file", "abcdefghij/", "яabcdefghij"]
        {
            assert!(!video_id(id));
        }
    }
}
