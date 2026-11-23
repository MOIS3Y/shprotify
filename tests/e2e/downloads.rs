//! Offline acquisition failures cannot break the existing shared library.

use std::fs;

use anyhow::{Result, ensure};
use reqwest::{Method, StatusCode};
use serde_json::json;

use crate::{
    media::acquired_wave,
    support::{Fixture, id, string, wave},
};

/// Fail a real provider process, retry successfully and stream managed audio.
#[tokio::test]
async fn failed_acquisition_is_isolated_and_retry_publishes_shared_audio()
-> Result<()> {
    let mut server = Fixture::new(true).await?;
    let alice = server.member("alice").await?;
    let bob = server.member("bob").await?;
    server.start().await?;
    let original = server.indexed_recording().await?;
    let original_stream = string(&original["files"][0], "stream_url")?;
    let capabilities = server.get("/api/v1/capabilities", "").await?;
    assert_eq!(capabilities["downloads_enabled"], true);
    let search = server
        .json(
            Method::POST,
            "/api/v1/me/downloads/searches",
            &alice,
            Some(json!({"kind":"query", "title":"Acquired song",
            "artists":["Chosen artist"]})),
            StatusCode::ACCEPTED,
        )
        .await?;
    let search_path = format!("/api/v1/me/downloads/{}", id(&search, "id")?);
    let done = server.wait_job(&search_path, &alice).await?;
    assert_eq!(done["status"], "completed");
    let sources_path = format!("{search_path}/sources");
    let sources = server.get(&sources_path, &alice).await?;
    server
        .json(
            Method::GET,
            &sources_path,
            &bob,
            None,
            StatusCode::NOT_FOUND,
        )
        .await?;
    let selected = json!({"source_id":id(&sources[0], "id")?});
    server
        .json(
            Method::POST,
            "/api/v1/me/downloads",
            &bob,
            Some(selected.clone()),
            StatusCode::NOT_FOUND,
        )
        .await?;
    let download = server
        .json(
            Method::POST,
            "/api/v1/me/downloads",
            &alice,
            Some(selected),
            StatusCode::ACCEPTED,
        )
        .await?;
    let job_path = format!("/api/v1/me/downloads/{}", id(&download, "id")?);
    let failed = server.wait_job(&job_path, &alice).await?;
    assert_eq!(failed["status"], "failed");
    ensure!(!failed.to_string().contains("fixture-secret-must-not-leak"));
    let original_audio = server
        .response(
            server.request(Method::GET, original_stream, &bob),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(original_audio.bytes().await?.as_ref(), wave());
    assert_eq!(server.get("/api/v1/recordings", &bob).await?["total"], 1);
    server
        .json(Method::GET, &job_path, &bob, None, StatusCode::NOT_FOUND)
        .await?;
    fs::write(server.path("provider-mode"), "ok")?;
    let retry = server
        .json(
            Method::POST,
            &format!("{job_path}/retry"),
            &alice,
            None,
            StatusCode::ACCEPTED,
        )
        .await?;
    assert_eq!(retry["retry_of"], download["id"]);
    let retry_path = format!("/api/v1/me/downloads/{}", id(&retry, "id")?);
    let completed = server.wait_job(&retry_path, &alice).await?;
    assert_eq!(completed["status"], "completed");
    let recording_id = id(&completed, "recording_id")?;
    let file_id = id(&completed, "file_id")?;
    let recording_path = format!("/api/v1/recordings/{recording_id}");
    let recording = server.get(&recording_path, &bob).await?;
    assert_eq!(recording["title"], "Acquired song");
    assert_eq!(recording["artists"][0]["name"], "Chosen artist");
    let tags = server
        .get(
            &format!("/api/v1/recordings/{recording_id}/files/{file_id}/tags"),
            &server.admin,
        )
        .await?;
    assert_eq!(tags["title"], "Offline song");
    let stream = string(&recording["files"][0], "stream_url")?;
    let audio = server
        .response(server.request(Method::GET, stream, &bob), StatusCode::OK)
        .await?;
    assert_eq!(audio.bytes().await?.as_ref(), acquired_wave());
    assert_eq!(server.get("/api/v1/recordings", &bob).await?["total"], 2);
    server.stop().await?;
    ensure!(
        !fs::read_to_string(server.path("server.log"))?
            .contains("fixture-secret-must-not-leak")
    );
    server.start().await?;
    assert_eq!(
        server.get(&retry_path, &alice).await?["status"],
        "completed"
    );
    let audio = server
        .response(server.request(Method::GET, stream, &bob), StatusCode::OK)
        .await?;
    assert_eq!(audio.bytes().await?.as_ref(), acquired_wave());
    assert_eq!(fs::read(server.path("music/song.wav"))?, wave());
    server.stop().await
}
