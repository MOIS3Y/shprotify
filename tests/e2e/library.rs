//! External collection indexing, catalog navigation and media delivery.

use anyhow::{Result, ensure};
use reqwest::{Method, StatusCode, header};
use serde_json::json;

use crate::support::{Fixture, cover, id, string, wave};

/// Scan external files, browse the catalog, upload a cover and seek audio.
#[tokio::test]
async fn collection_scan_browsing_artwork_and_streaming() -> Result<()> {
    let mut server = Fixture::new(false).await?;
    let member = server.member("listener").await?;
    server.start().await?;
    server
        .json(
            Method::GET,
            "/api/v1/recordings",
            "",
            None,
            StatusCode::UNAUTHORIZED,
        )
        .await?;
    server
        .json(
            Method::POST,
            "/api/v1/library/scans",
            &member,
            None,
            StatusCode::FORBIDDEN,
        )
        .await?;
    let scan = server.scan().await?;
    assert_eq!(scan["status"], "completed");
    assert_eq!(scan["imported"], 1);
    assert_eq!(scan["errors"], 0);
    let recordings = server
        .json(
            Method::GET,
            "/api/v1/recordings?has_audio=true&limit=1",
            &member,
            None,
            StatusCode::OK,
        )
        .await?;
    assert_eq!(recordings["total"], 1);
    assert_eq!(recordings["has_more"], false);
    let recording = &recordings["items"][0];
    let recording_id = id(recording, "id")?;
    assert_eq!(recording["title"], "Offline song");
    assert_eq!(recording["artists"][0]["name"], "Offline artist");
    assert_eq!(recording["duration_ms"], 1000);
    let stream = string(&recording["files"][0], "stream_url")?;
    let releases = server
        .json(
            Method::GET,
            "/api/v1/releases",
            &member,
            None,
            StatusCode::OK,
        )
        .await?;
    assert_eq!(releases["items"][0]["title"], "Offline album");
    let release_id = id(&releases["items"][0], "id")?;
    let tracks = server
        .json(
            Method::GET,
            &format!("/api/v1/releases/{release_id}/tracks"),
            &member,
            None,
            StatusCode::OK,
        )
        .await?;
    assert_eq!(tracks["items"][0]["recording_id"], recording_id);
    let search = server
        .json(
            Method::GET,
            "/api/v1/search?q=Offline",
            &member,
            None,
            StatusCode::OK,
        )
        .await?;
    assert_eq!(search["items"].as_array().unwrap().len(), 3);

    let metadata_path = format!("/api/v1/metadata/recording/{recording_id}");
    let metadata = server
        .json(
            Method::GET,
            &metadata_path,
            &server.admin,
            None,
            StatusCode::OK,
        )
        .await?;
    let png = cover()?;
    let upload = format!(
        "{metadata_path}/artwork?version={}",
        id(&metadata, "version")?
    );
    server
        .response(
            server
                .request(Method::PUT, &upload, &server.admin)
                .header(header::CONTENT_TYPE, "image/png")
                .body(png.clone()),
            StatusCode::OK,
        )
        .await?;
    let cover_path = format!("/api/v1/artwork/recording/{recording_id}");
    let artwork = server
        .response(
            server.request(Method::GET, &cover_path, &member),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(artwork.headers()[header::CONTENT_TYPE], "image/png");
    let etag = artwork.headers()[header::ETAG].clone();
    assert_eq!(artwork.bytes().await?.as_ref(), png);
    server
        .response(
            server
                .request(Method::GET, &cover_path, &member)
                .header(header::IF_NONE_MATCH, etag),
            StatusCode::NOT_MODIFIED,
        )
        .await?;

    server
        .response(
            server.request(Method::GET, stream, ""),
            StatusCode::UNAUTHORIZED,
        )
        .await?;
    let head = server
        .response(
            server.request(Method::HEAD, stream, &member),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(
        head.headers()[header::CONTENT_LENGTH],
        wave().len().to_string()
    );
    ensure!(head.bytes().await?.is_empty());
    let audio = server
        .response(server.request(Method::GET, stream, &member), StatusCode::OK)
        .await?;
    assert_eq!(audio.bytes().await?.as_ref(), wave());
    let range = server
        .response(
            server
                .request(Method::GET, stream, &member)
                .header(header::RANGE, "bytes=0-3"),
            StatusCode::PARTIAL_CONTENT,
        )
        .await?;
    assert_eq!(
        range.headers()[header::CONTENT_RANGE],
        format!("bytes 0-3/{}", wave().len())
    );
    assert_eq!(range.bytes().await?.as_ref(), b"RIFF");
    let history = server
        .json(
            Method::GET,
            "/api/v1/me/history",
            &member,
            None,
            StatusCode::OK,
        )
        .await?;
    assert_eq!(history["items"], json!([]));

    let scan = server.scan().await?;
    assert_eq!(scan["imported"], 0);
    assert_eq!(scan["unchanged"], 1);
    assert_eq!(std::fs::read(server.path("music/song.wav"))?, wave());
    let schema = server
        .json(Method::GET, "/openapi.json", "", None, StatusCode::OK)
        .await?;
    ensure!(schema["paths"]["/api/v1/recordings"].is_object());
    server.stop().await
}
