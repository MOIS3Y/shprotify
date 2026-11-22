//! Optional Chromaprint/AcoustID identification from a read-only descriptor.

use super::{Transport, musicbrainz};
use crate::library::enrichment::{EntityKind, Proposal, mbid};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{fs::File, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

/// Minimal bounded fpcalc JSON output; neither path nor fingerprint is logged.
#[derive(Deserialize)]
struct Fingerprint {
    /// Total duration in seconds as reported by the decoder.
    duration: f64,
    /// Compressed Chromaprint representation.
    fingerprint: String,
}

/// Execute without a shell; descriptor stdin prevents pathname substitution.
async fn fingerprint(binary: &str, file: File) -> Result<Fingerprint> {
    fingerprint_with_timeout(binary, file, Duration::from_secs(60)).await
}

/// Parameterized deadline keeps process termination tests fast and repeatable.
async fn fingerprint_with_timeout(
    binary: &str,
    file: File,
    timeout: Duration,
) -> Result<Fingerprint> {
    let mut child = Command::new(binary)
        .args(["-json", "-length", "120", "/proc/self/fd/0"])
        .stdin(Stdio::from(file))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = child.stdout.take().context("Missing fpcalc stdout")?;
    let result = tokio::time::timeout(timeout, async {
        let mut data = Vec::new();
        (&mut stdout)
            .take(128 * 1024 + 1)
            .read_to_end(&mut data)
            .await?;
        ensure!(data.len() <= 128 * 1024, "Fingerprint output too large");
        ensure!(child.wait().await?.success(), "fpcalc failed");
        let result: Fingerprint = serde_json::from_slice(&data)?;
        ensure!(
            result.duration.is_finite()
                && result.duration >= 1.0
                && result.duration <= 86400.0
                && !result.fingerprint.is_empty(),
            "Invalid fingerprint result"
        );
        Ok::<_, anyhow::Error>(result)
    })
    .await;
    // Explicitly reap failures/timeouts as well as killing on cancellation.
    if !matches!(&result, Ok(Ok(_))) {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result.context("Fingerprint operation timed out")?
}

/// Identify audio and resolve up to five distinct Recording IDs through MB.
pub(crate) async fn identify(
    http: &Transport,
    binary: &str,
    key: &str,
    file: File,
) -> Result<Vec<Proposal>> {
    let fingerprint = fingerprint(binary, file).await?;
    let ids =
        lookup(http, "https://api.acoustid.org/v2/lookup", key, fingerprint)
            .await?;
    let mut proposals = Vec::new();
    for (id, score) in ids {
        let mut found =
            musicbrainz::search(http, EntityKind::Recording, "", Some(&id))
                .await?;
        for candidate in &mut found {
            candidate.source = "acoustid".into();
            candidate.score = Some(score);
        }
        proposals.extend(found);
    }
    Ok(proposals)
}

/// Bounded form POST keeps the application key out of URL diagnostics.
async fn lookup(
    http: &Transport,
    endpoint: &str,
    key: &str,
    fingerprint: Fingerprint,
) -> Result<Vec<(String, f64)>> {
    http.pace().await;
    let response = http
        .client
        .post(endpoint)
        .form(&[
            ("client", key.to_owned()),
            ("meta", "recordingids".into()),
            (
                "duration",
                (fingerprint.duration.round() as u64).to_string(),
            ),
            ("fingerprint", fingerprint.fingerprint),
        ])
        .send()
        .await?;
    let data = Transport::bytes(response, 1024 * 1024).await?;
    let value: Value = serde_json::from_slice(&data)?;
    ensure!(value["status"] == "ok", "AcoustID lookup failed");
    let results = value["results"].as_array().context("Missing results")?;
    let mut ids = Vec::new();
    for result in results {
        let score = result["score"].as_f64().context("Missing match score")?;
        ensure!((0.0..=1.0).contains(&score), "Invalid match score");
        if let Some(recordings) = result["recordings"].as_array() {
            for recording in recordings {
                let id = mbid(
                    recording["id"]
                        .as_str()
                        .context("Missing recording ID")?,
                )
                .map_err(anyhow::Error::msg)?;
                if ids.len() < 5 && !ids.iter().any(|(v, _)| v == &id) {
                    ids.push((id, score));
                }
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    //! External process boundaries without invoking a shell in production.
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Test executable asserts descriptor stdin and deterministic arguments.
    #[tokio::test]
    async fn fingerprints_descriptor_and_bounds_output_and_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("source");
        std::fs::write(&audio, b"test-media").unwrap();
        let binary = dir.path().join("fake-fpcalc");
        std::fs::write(
            &binary,
            concat!(
                "#!/bin/sh\n",
                "test \"$1\" = -json || exit 2\n",
                "test \"$4\" = /proc/self/fd/0 || exit 2\n",
                "test \"$(cat)\" = test-media || exit 2\n",
                "printf '%s' '{\"duration\":42,\"fingerprint\":\"abc\"}'\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(
            &binary,
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let result =
            fingerprint(binary.to_str().unwrap(), File::open(&audio).unwrap())
                .await
                .unwrap();
        assert_eq!(result.duration, 42.0);
        assert_eq!(result.fingerprint, "abc");
        std::fs::write(&binary, "#!/bin/sh\nexec sleep 10\n").unwrap();
        let started = std::time::Instant::now();
        assert!(
            fingerprint_with_timeout(
                binary.to_str().unwrap(),
                File::open(&audio).unwrap(),
                Duration::from_millis(50)
            )
            .await
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        std::fs::write(&binary, "#!/bin/sh\nexec head -c 131073 /dev/zero\n")
            .unwrap();
        assert!(
            fingerprint(binary.to_str().unwrap(), File::open(&audio).unwrap())
                .await
                .is_err()
        );
    }
    /// Verify form transport, score validation and empty-match handling.
    #[tokio::test]
    async fn posts_fingerprint_without_key_in_url() {
        use axum::{Json, Router, extract::OriginalUri, routing::post};
        use serde_json::json;
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/lookup",
                post(
                    |OriginalUri(uri): OriginalUri, body: String| async move {
                        assert!(uri.query().is_none());
                        assert!(body.contains("client=test-key"));
                        assert!(body.contains("fingerprint=abc"));
                        assert!(body.contains("duration=42"));
                        Json(json!({"status":"ok","results":[{"score":0.95,
                    "recordings":[{
                        "id":"12345678-1234-1234-1234-123456789abc"}]}]}))
                    },
                ),
            )
            .route(
                "/error",
                post(|| async { Json(json!({"status":"error"})) }),
            )
            .route(
                "/empty",
                post(|| async {
                    Json(json!({
              "status":"ok","results":[]}))
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http =
            Transport::new("shprotify/test (test@example.org)").unwrap();
        let fp = || Fingerprint {
            duration: 42.0,
            fingerprint: "abc".into(),
        };
        let ids = lookup(
            &http,
            &format!("http://{address}/lookup"),
            "test-key",
            fp(),
        )
        .await
        .unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].1, 0.95);
        assert!(
            lookup(
                &http,
                &format!("http://{address}/error"),
                "test-key",
                fp()
            )
            .await
            .is_err()
        );
        assert!(
            lookup(
                &http,
                &format!("http://{address}/empty"),
                "test-key",
                fp()
            )
            .await
            .unwrap()
            .is_empty()
        );
        server.abort();
    }
}
