//! Personal collections remain isolated, retry-safe and durable on restart.

use anyhow::Result;
use reqwest::{Method, StatusCode};
use serde_json::json;

use crate::support::{Fixture, id};

/// Keep favorites, playlists, queue and history private across a full restart.
#[tokio::test]
async fn personal_collections_are_isolated_and_survive_restart() -> Result<()>
{
    let mut server = Fixture::new(false).await?;
    let alice = server.member("alice").await?;
    let bob = server.member("bob").await?;
    server.start().await?;
    let recording = server.indexed_recording().await?;
    let recording_id = id(&recording, "id")?;
    let reaction = format!("/api/v1/me/recordings/{recording_id}/reaction");
    for assessment in ["like", "dislike", "like"] {
        server
            .json(
                Method::PUT,
                &reaction,
                &alice,
                Some(json!({"reaction":assessment})),
                StatusCode::OK,
            )
            .await?;
        let favorites = server.get("/api/v1/me/favorites", &alice).await?;
        assert_eq!(
            favorites["items"].as_array().unwrap().len(),
            usize::from(assessment == "like")
        );
    }
    assert_eq!(
        server.get("/api/v1/me/favorites", &bob).await?["items"],
        json!([])
    );
    assert_eq!(server.get(&reaction, &bob).await?["reaction"], json!(null));

    let playlist = server
        .json(
            Method::POST,
            "/api/v1/me/playlists",
            &alice,
            Some(json!({"name":"Offline favorites", "description":"E2E"})),
            StatusCode::CREATED,
        )
        .await?;
    let playlist_id = id(&playlist, "id")?;
    let owned = format!("/api/v1/me/playlists/{playlist_id}");
    let tracks = format!("{owned}/tracks");
    let playlist = server
        .json(
            Method::PUT,
            &tracks,
            &alice,
            Some(json!({"version":playlist["version"],
            "recording_ids":[recording_id,recording_id]})),
            StatusCode::OK,
        )
        .await?;
    let entries = server.get(&tracks, &alice).await?;
    assert_eq!(entries["items"].as_array().unwrap().len(), 2);
    assert_eq!(entries["items"][1]["recording_id"], recording_id);
    for path in [&owned, &tracks] {
        server
            .json(Method::GET, path, &bob, None, StatusCode::NOT_FOUND)
            .await?;
    }
    let public = format!("/api/v1/playlists/{playlist_id}");
    server
        .json(Method::GET, &public, &bob, None, StatusCode::NOT_FOUND)
        .await?;
    let published = server
        .json(
            Method::PUT,
            &format!("{owned}/visibility"),
            &alice,
            Some(json!({"version":playlist["version"], "is_public":true})),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(
        server.get(&public, &bob).await?["name"],
        "Offline favorites"
    );
    server
        .json(
            Method::PUT,
            &tracks,
            &bob,
            Some(json!({"version":published["version"],"recording_ids":[]})),
            StatusCode::NOT_FOUND,
        )
        .await?;
    server
        .json(
            Method::PUT,
            &format!("{owned}/visibility"),
            &alice,
            Some(json!({"version":published["version"],"is_public":false})),
            StatusCode::OK,
        )
        .await?;
    server
        .json(Method::GET, &public, &bob, None, StatusCode::NOT_FOUND)
        .await?;

    let queue = server
        .json(
            Method::PUT,
            "/api/v1/me/queue",
            &alice,
            Some(
                json!({"version":0,"recording_ids":[recording_id,recording_id],
            "current_position":2,"position_ms":400}),
            ),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(queue["current_position"], 2);
    assert_eq!(queue["items"].as_array().unwrap().len(), 2);
    server
        .json(
            Method::PUT,
            "/api/v1/me/queue/playback",
            &alice,
            Some(json!({"version":0,"current_position":1,"position_ms":0})),
            StatusCode::CONFLICT,
        )
        .await?;
    assert_eq!(server.get("/api/v1/me/queue", &alice).await?, queue);
    assert_eq!(server.get("/api/v1/me/queue", &bob).await?["version"], 0);

    let facts = json!({
        "event_id":"offline-play-1", "recording_id":recording_id,
        "started_at":1700000000,"listened_ms":900,"duration_ms":1000
    });
    let event = server
        .json(
            Method::POST,
            "/api/v1/me/history",
            &alice,
            Some(facts.clone()),
            StatusCode::CREATED,
        )
        .await?;
    assert_eq!(event["kind"], "full");
    let retry = server
        .json(
            Method::POST,
            "/api/v1/me/history",
            &alice,
            Some(facts),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(retry, event);
    let history_path = format!("/api/v1/me/history/{}", id(&event, "id")?);
    server
        .json(
            Method::GET,
            &history_path,
            &bob,
            None,
            StatusCode::NOT_FOUND,
        )
        .await?;
    assert_eq!(
        server.get("/api/v1/me/history", &bob).await?["items"],
        json!([])
    );

    let mut snapshots = Vec::new();
    for path in [
        "/api/v1/me/favorites",
        &owned,
        &tracks,
        "/api/v1/me/queue",
        "/api/v1/me/history",
        &format!("/api/v1/recordings/{recording_id}"),
    ] {
        snapshots.push((path.to_owned(), server.get(path, &alice).await?));
    }
    server.stop().await?;
    server.start().await?;
    for (path, before) in snapshots {
        assert_eq!(server.get(&path, &alice).await?, before, "{path}");
    }
    let retry = server.json(Method::POST, "/api/v1/me/history", &alice,
        Some(json!({"event_id":"offline-play-1", "recording_id":recording_id,
            "started_at":1700000000,"listened_ms":900,"duration_ms":1000})),
        StatusCode::OK).await?;
    assert_eq!(retry["id"], event["id"]);
    assert_eq!(
        server.get("/api/v1/me/history", &alice).await?["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    server
        .json(Method::DELETE, &reaction, &alice, None, StatusCode::OK)
        .await?;
    assert_eq!(
        server.get("/api/v1/me/favorites", &alice).await?["items"],
        json!([])
    );
    server.stop().await
}
