//! Personal recommendations and decisions work without upstream APIs.

use anyhow::Result;
use reqwest::{Method, StatusCode};
use serde_json::json;

use crate::support::{Fixture, id};

/// Generate a local feed, export a playlist and retain private feedback.
#[tokio::test]
async fn local_discovery_and_wishlist_work_without_external_services()
-> Result<()> {
    let mut server = Fixture::new(false).await?;
    let alice = server.member("alice").await?;
    let bob = server.member("bob").await?;
    server.start().await?;
    let recording = server.indexed_recording().await?;
    let recording_id = id(&recording, "id")?;
    server
        .json(
            Method::PUT,
            &format!("/api/v1/me/recordings/{recording_id}/reaction"),
            &alice,
            Some(json!({"reaction":"like"})),
            StatusCode::OK,
        )
        .await?;
    let profile = server.get("/api/v1/me/discovery/profile", &alice).await?;
    assert_eq!(profile["artists"][0]["name"], "Offline artist");
    let profile = server.get("/api/v1/me/discovery/profile", &bob).await?;
    assert_eq!(profile["artists"], json!([]));
    let job = server
        .json(
            Method::POST,
            "/api/v1/me/discovery/feeds",
            &alice,
            Some(json!({"kind":"rediscover"})),
            StatusCode::ACCEPTED,
        )
        .await?;
    let job_path = format!("/api/v1/me/discovery/jobs/{}", id(&job, "id")?);
    assert_eq!(
        server.wait_job(&job_path, &alice).await?["status"],
        "completed"
    );
    let feed = format!("/api/v1/me/discovery/feeds/{}", id(&job, "feed_id")?);
    server
        .json(Method::GET, &feed, &bob, None, StatusCode::NOT_FOUND)
        .await?;
    server
        .json(Method::GET, &job_path, &bob, None, StatusCode::NOT_FOUND)
        .await?;
    let contents = server.get(&format!("{feed}/items"), &alice).await?;
    assert_eq!(contents["total"], 1);
    let candidate = &contents["items"][0];
    let candidate_id = id(candidate, "id")?;
    assert_eq!(candidate["candidate"]["provider"], "local");
    assert_eq!(candidate["local_id"], recording_id);
    assert_eq!(candidate["has_registered_audio"], true);
    let exported = server
        .json(
            Method::POST,
            &format!("{feed}/playlist"),
            &alice,
            Some(json!({"version":contents["version"],
            "name":"Rediscovered offline"})),
            StatusCode::CREATED,
        )
        .await?;
    assert_eq!(exported["included"], 1);
    assert_eq!(exported["skipped"], 0);
    let playlist_id = id(&exported["playlist"], "id")?;
    let tracks = server
        .get(
            &format!("/api/v1/me/playlists/{playlist_id}/tracks"),
            &alice,
        )
        .await?;
    assert_eq!(tracks["items"][0]["recording_id"], recording_id);

    let feedback =
        format!("/api/v1/me/discovery/candidates/{candidate_id}/feedback");
    server.json(Method::PUT, &feedback, &alice,
        Some(json!({"version":0,"reaction":null,"hidden":true,"wanted":true})),
        StatusCode::OK).await?;
    assert_eq!(
        server.get(&format!("{feed}/items"), &alice).await?["total"],
        0
    );
    assert_eq!(
        server
            .get(&format!("{feed}/items?include_hidden=true"), &alice)
            .await?["total"],
        1
    );
    let wishlist = server.get("/api/v1/me/discovery/wishlist", &alice).await?;
    assert_eq!(wishlist["items"][0]["id"], candidate_id);
    assert_eq!(
        server.get("/api/v1/me/discovery/wishlist", &bob).await?["items"],
        json!([])
    );
    server
        .json(
            Method::POST,
            "/api/v1/me/discovery/feeds",
            &alice,
            Some(json!({"kind":"chart"})),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await?;
    server.stop().await?;
    server.start().await?;
    assert_eq!(
        server.get("/api/v1/me/discovery/wishlist", &alice).await?,
        wishlist
    );
    assert_eq!(
        server.get(&feed, &alice).await?["version"],
        contents["version"]
    );
    server.stop().await
}
