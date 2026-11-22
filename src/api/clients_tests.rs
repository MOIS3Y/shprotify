//! Real SQLite/router contracts for catalog browsing and client media access.

use crate::{
    auth,
    config::{ClientSettings, DocsConfig},
    db,
    downloads::storage::Storage,
    jobs::{ScanService, removals::RemovalService},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

/// Complete isolated HTTP fixture, including server-owned media bytes.
struct Fixture {
    /// Keep the filesystem alive throughout asynchronous work.
    _dir: tempfile::TempDir,
    /// Migrated real catalog, shared by all HTTP handlers.
    pool: SqlitePool,
    /// Production routes and state.
    app: Router,
    /// Administrator parent key.
    token: String,
    /// Ordinary account key for permission/privacy tests.
    member: String,
    /// Drained explicitly before checking durable results.
    removals: RemovalService,
    /// Managed boundary, also used to simulate interrupted deletion.
    storage: Storage,
    /// Generated hash and file path for a verified acquired object.
    hash: String,
}

/// Seed relations with repeated recordings and deliberately unavailable audio.
async fn setup(public: bool) -> Fixture {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let pool = db::connect(&dir.path().join("library.db")).await.unwrap();
    let token = auth::token().unwrap();
    let member = auth::token().unwrap();
    db::users::create(&pool, "admin", &auth::digest(&token))
        .await
        .unwrap();
    db::users::set_role(&pool, "admin", auth::Role::Admin)
        .await
        .unwrap();
    db::users::create(&pool, "member", &auth::digest(&member))
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO artists(id,name) VALUES(1,'Zulu'),(2,'Alpha');
        INSERT INTO recordings(id,title) VALUES
            (1,'Beta'),(2,'Alpha'),(3,'Alpha');
        INSERT INTO recording_artists VALUES(1,2,0),(1,1,1),(2,2,0);
        INSERT INTO releases(id,title,kind) VALUES
            (1,'Album','album'),(2,'Single','single');
        INSERT INTO release_artists VALUES(1,2,0),(2,1,0);
        INSERT INTO release_tracks
            (release_id,recording_id,disc_number,position) VALUES
            (1,1,1,1),(1,2,1,2),(1,1,2,1),(2,3,1,1);",
    )
    .execute(&pool)
    .await
    .unwrap();
    let storage = Storage::prepare(&dir.path().join("managed"), &[]).unwrap();
    let hash = Sha256::digest(b"music")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let path = storage.object(&hash, "wav");
    std::fs::write(&path, b"music").unwrap();
    sqlx::query(
        "INSERT INTO audio_files \
        (id,recording_id,path,format,duration_ms,size_bytes) \
        VALUES(1,1,?,'wav',1234,5)",
    )
    .bind(path.to_str().unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO audio_file_hashes VALUES(1,?)")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO downloaded_sources VALUES('abcdefghijk',1,1)")
        .execute(&pool)
        .await
        .unwrap();
    let scans = ScanService::new(pool.clone(), vec![]);
    let metadata = crate::jobs::metadata::MetadataService::new(
        pool.clone(),
        scans.roots(),
        crate::config::MetadataConfig::default(),
    )
    .unwrap();
    let discovery = crate::jobs::discovery::DiscoveryService::new(
        pool.clone(),
        crate::config::DiscoveryConfig::default(),
    )
    .unwrap();
    let downloads = crate::jobs::downloads::DownloadService::new(
        pool.clone(),
        crate::config::DownloadsConfig::default(),
        Some(storage.clone()),
    );
    let removals = RemovalService::new(pool.clone(), Some(storage.clone()));
    let app = crate::api::router_with_services(
        &crate::config::AuthConfig::default(),
        &DocsConfig { enabled: true },
        &pool,
        crate::api::Services {
            scans,
            metadata,
            discovery,
            downloads,
            removals: removals.clone(),
            automation: crate::config::AutomationConfig::default(),
            clients: ClientSettings {
                public_catalog: public,
                cors_origins: vec!["https://player.example".into()],
                ticket_ttl_seconds: 300,
            },
        },
    );
    Fixture {
        _dir: dir,
        pool,
        app,
        token,
        member,
        removals,
        storage,
        hash,
    }
}

/// Send JSON through the production stack and decode JSON responses only.
async fn request(
    f: &Fixture,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder =
            builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = match body {
        Some(body) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = f
        .app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

/// Filters are applied before paging and rich cards retain ordered credits.
#[tokio::test]
async fn catalog_pages_have_counts_filters_audio_and_stable_sorting() {
    let f = setup(true).await;
    let (status, _, page) = request(
        &f,
        "GET",
        "/api/v1/recordings?artist_id=2&sort=title&limit=1",
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(page["total"], 2);
    assert_eq!(page["has_more"], true);
    assert_eq!(page["items"][0]["id"], 2);
    let (_, _, page) = request(
        &f,
        "GET",
        "/api/v1/artists/2/recordings?has_audio=true",
        None,
        None,
    )
    .await;
    let card = &page["items"][0];
    assert_eq!(page["total"], 1);
    assert_eq!(card["artist_ids"], json!([2, 1]));
    assert_eq!(card["artists"][0]["name"], "Alpha");
    assert_eq!(card["files"][0]["duration_ms"], 1234);
    assert!(card["files"][0].get("path").is_none());
    let (_, _, page) = request(
        &f,
        "GET",
        "/api/v1/recordings?sort=duration&order=desc",
        None,
        None,
    )
    .await;
    assert_eq!(page["items"][0]["id"], 1);
    assert_eq!(page["items"][1]["id"], 3);
    let (_, _, page) = request(
        &f,
        "GET",
        "/api/v1/recordings?release_id=1&offset=50",
        None,
        None,
    )
    .await;
    assert_eq!(page["total"], 2);
    assert_eq!(page["has_more"], false);
    for path in [
        "/api/v1/artists/999/recordings",
        "/api/v1/artists/999/releases",
        "/api/v1/recordings/999/releases",
    ] {
        assert_eq!(request(&f, "GET", path, None, None).await.0, 404);
    }
    for path in [
        "/api/v1/recordings?sort=name",
        "/api/v1/artists?sort=duration",
        "/api/v1/releases?artist_id=0",
        "/api/v1/artists/1/recordings?artist_id=2",
    ] {
        assert_eq!(request(&f, "GET", path, None, None).await.0, 400);
    }
    f.pool.close().await;
}

/// Release projections count occurrences; repeated recordings keep positions.
#[tokio::test]
async fn release_tracks_batch_recordings_and_distinguish_partial_audio() {
    let f = setup(true).await;
    let (_, _, release) =
        request(&f, "GET", "/api/v1/releases/1", None, None).await;
    assert_eq!(release["track_count"], 3);
    assert_eq!(release["available_track_count"], 2);
    assert_eq!(release["is_complete"], false);
    assert_eq!(release["duration_ms"], Value::Null);
    let (_, _, tracks) =
        request(&f, "GET", "/api/v1/releases/1/tracks", None, None).await;
    assert_eq!(tracks["total"], 3);
    assert_eq!(tracks["items"][2]["recording_id"], 1);
    assert_eq!(tracks["items"][2]["disc_number"], 2);
    assert_eq!(tracks["items"][2]["recording"]["files"][0]["format"], "wav");
    let (_, _, releases) = request(
        &f,
        "GET",
        "/api/v1/recordings/1/releases?kind=album",
        None,
        None,
    )
    .await;
    assert_eq!(releases["total"], 1);
    let (_, _, stats) =
        request(&f, "GET", "/api/v1/library/stats", None, None).await;
    assert_eq!(stats["recordings"], 3);
    assert_eq!(stats["recordings_with_audio"], 1);
    f.pool.close().await;
}

/// Every catalog family is guarded before parsing invalid query/path input.
#[tokio::test]
async fn private_catalog_requires_bearer_and_capabilities_stay_public() {
    let f = setup(false).await;
    for path in [
        "/api/v1/artists",
        "/api/v1/releases",
        "/api/v1/recordings",
        "/api/v1/releases/1/tracks",
        "/api/v1/recordings/1/files",
        "/api/v1/recordings/1/files/1/tags",
        "/api/v1/search?q=Alpha",
        "/api/v1/artwork/release/1",
        "/api/v1/library/stats",
        "/api/v1/recordings?limit=-1",
    ] {
        let (status, headers, _) = request(&f, "GET", path, None, None).await;
        assert_eq!(status, 401, "{path}");
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    }
    let (status, headers, _) =
        request(&f, "GET", "/api/v1/recordings", Some(&f.member), None).await;
    assert_eq!(status, 200);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    let (_, _, cap) =
        request(&f, "GET", "/api/v1/capabilities", None, None).await;
    assert_eq!(cap["public_catalog"], false);
    assert_eq!(cap["pagination"]["maximum"], 100);
    assert!(!cap.to_string().contains("managed"));
    assert_eq!(
        request(&f, "GET", "/api/v1/health", None, None).await.0,
        200
    );
    f.pool.close().await;
}

/// Preflight precedes authentication and allows only exact operator origins.
#[tokio::test]
async fn cors_allows_explicit_origins_without_cookie_credentials() {
    let f = setup(false).await;
    for (origin, allowed) in [
        ("https://player.example", true),
        ("https://attacker.example", false),
    ] {
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/v1/recordings")
                    .header(header::ORIGIN, origin)
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                    .header(
                        header::ACCESS_CONTROL_REQUEST_HEADERS,
                        "authorization",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(
            response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            allowed
        );
        assert!(
            !response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
        );
        assert!(
            response.headers()[header::VARY]
                .to_str()
                .unwrap()
                .to_ascii_lowercase()
                .contains("origin")
        );
    }
    f.pool.close().await;
}

/// Links support range/HEAD but never authorize catalog or other files.
#[tokio::test]
async fn media_ticket_is_scoped_reusable_and_revoked_with_parent() {
    let f = setup(false).await;
    let path = "/api/v1/recordings/1/files/1/playback-ticket";
    assert_eq!(request(&f, "POST", path, None, None).await.0, 401);
    let (status, headers, ticket) =
        request(&f, "POST", path, Some(&f.member), None).await;
    assert_eq!(status, 200);
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
    let url = ticket["stream_url"].as_str().unwrap();
    for method in ["GET", "HEAD"] {
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(url)
                    .header(header::RANGE, "bytes=1-2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if method == "HEAD" { 200 } else { 206 }
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(
            body.as_ref(),
            if method == "HEAD" {
                &b""[..]
            } else {
                &b"us"[..]
            }
        );
    }
    assert_eq!(request(&f, "GET", url, Some("invalid"), None).await.0, 401);
    let wrong = url.replace("/files/1/", "/files/2/");
    assert_eq!(request(&f, "GET", &wrong, None, None).await.0, 401);
    let query = url.split_once('?').unwrap().1;
    assert_eq!(
        request(
            &f,
            "GET",
            &format!("/api/v1/recordings?{query}"),
            None,
            None
        )
        .await
        .0,
        401
    );
    let token = query.strip_prefix("ticket=").unwrap();
    assert_eq!(
        request(&f, "GET", "/api/v1/me", Some(token), None).await.0,
        401
    );
    sqlx::query("DELETE FROM user_keys WHERE digest=?")
        .bind(auth::digest(&f.member))
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(request(&f, "GET", url, None, None).await.0, 401);
    f.pool.close().await;
}

/// Batch reaction reads remain personal and distinguish neutral/missing IDs.
#[tokio::test]
async fn batch_reactions_preserve_order_privacy_and_validation() {
    let f = setup(true).await;
    db::reactions::set(
        &f.pool,
        1,
        1,
        Some(crate::library::reactions::Reaction::Like),
    )
    .await
    .unwrap();
    let path = "/api/v1/me/recordings/reactions";
    let (_, _, batch) = request(
        &f,
        "POST",
        path,
        Some(&f.member),
        Some(json!({"recording_ids":[3,999,1]})),
    )
    .await;
    assert_eq!(batch["missing_ids"], json!([999]));
    assert_eq!(batch["items"][0]["recording_id"], 3);
    assert_eq!(batch["items"][1]["reaction"], Value::Null);
    for ids in [json!([1, 1]), json!([0]), json!([-1]), json!(vec![1; 101])] {
        assert_eq!(
            request(
                &f,
                "POST",
                path,
                Some(&f.member),
                Some(json!({"recording_ids":ids}))
            )
            .await
            .0,
            422
        );
    }
    f.pool.close().await;
}

/// Unregistering keeps recording identity, likes and physical bytes intact.
#[tokio::test]
async fn unregister_file_preserves_personal_data_and_requires_admin() {
    let f = setup(true).await;
    db::reactions::set(
        &f.pool,
        2,
        1,
        Some(crate::library::reactions::Reaction::Like),
    )
    .await
    .unwrap();
    let path = "/api/v1/recordings/1/files/1";
    assert_eq!(
        request(&f, "DELETE", path, Some(&f.member), None).await.0,
        403
    );
    let (status, headers, job) =
        request(&f, "DELETE", path, Some(&f.token), None).await;
    assert_eq!(status, 202);
    assert!(
        headers[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("/api/v1/library/file-removals/")
    );
    f.removals.close().await;
    let operation = db::removals::find(&f.pool, job["id"].as_i64().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operation.status, "completed");
    assert!(f.storage.object(&f.hash, "wav").exists());
    assert!(
        db::audio_files::find(&f.pool, 1, 1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(db::recordings::find(&f.pool, 1).await.unwrap().is_some());
    assert!(
        db::reactions::find(&f.pool, 2, 1)
            .await
            .unwrap()
            .unwrap()
            .reaction
            .is_some()
    );
    let (_, _, card) =
        request(&f, "GET", "/api/v1/recordings/1", None, None).await;
    assert_eq!(card["has_audio"], false);
    f.pool.close().await;
}

/// Recovery after unlink completes SQLite; altered bytes fail closed.
#[tokio::test]
async fn physical_removal_checks_hash_and_recovers_after_unlink() {
    let f = setup(true).await;
    let intent = db::removals::start(&f.pool, 1, 1, 1, true)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db::audio_files::find(&f.pool, 1, 1)
            .await
            .unwrap()
            .is_none()
    );
    f.storage
        .remove(&intent.path, intent.sha256.as_ref().unwrap())
        .unwrap();
    f.removals.recover().await.unwrap();
    assert_eq!(
        db::removals::find(&f.pool, intent.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "completed"
    );
    assert!(!f.storage.object(&f.hash, "wav").exists());
    f.pool.close().await;
    let f = setup(true).await;
    std::fs::write(f.storage.object(&f.hash, "wav"), b"changed").unwrap();
    let (status, _, job) = request(
        &f,
        "DELETE",
        "/api/v1/recordings/1/files/1?remove_file=true",
        Some(&f.token),
        None,
    )
    .await;
    assert_eq!(status, 202);
    f.removals.close().await;
    assert_eq!(
        db::removals::find(&f.pool, job["id"].as_i64().unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
    assert!(
        db::audio_files::find(&f.pool, 1, 1)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        std::fs::read(f.storage.object(&f.hash, "wav")).unwrap(),
        b"changed"
    );
    f.pool.close().await;
}

/// Session expiry caps tickets; revocation is checked without waiting for TTL.
#[tokio::test]
async fn media_ticket_respects_parent_expiry_and_independent_keys() {
    let f = setup(true).await;
    let session = auth::session_token().unwrap();
    let expiry: i64 = sqlx::query_scalar("SELECT unixepoch()+60")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_sessions VALUES(?,2,?)")
        .bind(auth::digest(&session))
        .bind(expiry)
        .execute(&f.pool)
        .await
        .unwrap();
    let path = "/api/v1/recordings/1/files/1/playback-ticket";
    let (_, _, session_ticket) =
        request(&f, "POST", path, Some(&session), None).await;
    assert_eq!(session_ticket["expires_at"], expiry);
    let (_, _, key_ticket) =
        request(&f, "POST", path, Some(&f.member), None).await;
    sqlx::query("DELETE FROM user_sessions WHERE digest=?")
        .bind(auth::digest(&session))
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &f,
            "GET",
            session_ticket["stream_url"].as_str().unwrap(),
            None,
            None
        )
        .await
        .0,
        401
    );
    let url = key_ticket["stream_url"].as_str().unwrap();
    assert_eq!(request(&f, "GET", url, None, None).await.0, 200);
    sqlx::query("UPDATE media_tickets SET expires_at=unixepoch()-1")
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(request(&f, "GET", url, None, None).await.0, 401);
    f.pool.close().await;
}

/// Closed deployments advertise bearer security in the actual served schema.
#[tokio::test]
async fn private_catalog_openapi_matches_runtime_policy() {
    let f = setup(false).await;
    let (_, _, schema) = request(&f, "GET", "/openapi.json", None, None).await;
    for path in [
        "/artists",
        "/releases/{id}",
        "/recordings/{id}",
        "/artwork/{kind}/{id}",
        "/library/stats",
        "/search",
    ] {
        let operation = &schema["paths"][format!("/api/v1{path}")]["get"];
        assert!(
            operation["security"][0]["personal_key"].is_array(),
            "{path}"
        );
        assert!(operation["responses"]["401"].is_object());
    }
    assert_eq!(schema["paths"][
        "/api/v1/recordings/{id}/files/{file_id}/stream"]
        ["get"]["security"].as_array().unwrap().len(),2);
    f.pool.close().await;
}

/// Successful physical removal retains the catalog identity and safe audit.
#[tokio::test]
async fn physical_removal_finishes_and_history_is_admin_only() {
    let f = setup(true).await;
    let (status, _, job) = request(
        &f,
        "DELETE",
        "/api/v1/recordings/1/files/1?remove_file=true",
        Some(&f.token),
        None,
    )
    .await;
    assert_eq!(status, 202);
    f.removals.close().await;
    let path = format!("/api/v1/library/file-removals/{}", job["id"]);
    assert_eq!(
        request(&f, "GET", &path, Some(&f.member), None).await.0,
        403
    );
    let (_, _, job) = request(&f, "GET", &path, Some(&f.token), None).await;
    assert_eq!(job["status"], "completed");
    assert!(job.get("path").is_none());
    assert!(job.get("sha256").is_none());
    assert!(!f.storage.object(&f.hash, "wav").exists());
    assert!(db::recordings::find(&f.pool, 1).await.unwrap().is_some());
    f.pool.close().await;
}

/// Even a registered/hash-verified external file cannot be physically removed.
#[tokio::test]
async fn removal_refuses_external_paths_and_symlinked_objects() {
    let f = setup(true).await;
    let external = f._dir.path().join("external.wav");
    std::fs::write(&external, b"music").unwrap();
    sqlx::query("UPDATE audio_files SET path=? WHERE id=1")
        .bind(external.to_str().unwrap())
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        request(
            &f,
            "DELETE",
            "/api/v1/recordings/1/files/1?remove_file=true",
            Some(&f.token),
            None
        )
        .await
        .0,
        409
    );
    assert!(external.exists());
    let object = f.storage.object(&f.hash, "wav");
    std::fs::remove_file(&object).unwrap();
    std::os::unix::fs::symlink(&external, &object).unwrap();
    sqlx::query("UPDATE audio_files SET path=? WHERE id=1")
        .bind(object.to_str().unwrap())
        .execute(&f.pool)
        .await
        .unwrap();
    let id = f.removals.start(1, 1, 1, true).await.unwrap();
    f.removals.close().await;
    assert_eq!(
        db::removals::find(&f.pool, id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "failed"
    );
    assert_eq!(std::fs::read(&external).unwrap(), b"music");
    assert!(object.symlink_metadata().unwrap().file_type().is_symlink());
    f.pool.close().await;
}

/// Personal lists can resolve display/audio information with one page request.
#[tokio::test]
async fn recording_batch_reports_missing_and_rejects_ambiguous_input() {
    let f = setup(true).await;
    let (status, _, batch) = request(
        &f,
        "GET",
        "/api/v1/recordings/batch?ids=3,999,1",
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(batch["items"][0]["id"], 3);
    assert_eq!(batch["items"][1]["files"][0]["id"], 1);
    assert_eq!(batch["missing_ids"], json!([999]));
    for query in ["ids=1,1", "ids=0", "ids=1,no", "ids=1&user_id=2"] {
        assert_eq!(
            request(
                &f,
                "GET",
                &format!("/api/v1/recordings/batch?{query}"),
                None,
                None
            )
            .await
            .0,
            400
        );
    }
    let (_, _, empty) =
        request(&f, "GET", "/api/v1/recordings/batch?ids=", None, None).await;
    assert_eq!(empty["items"], json!([]));
    f.pool.close().await;
}

/// An admitted deletion locks identity and immediately hides availability.
#[tokio::test]
async fn pending_removal_blocks_relink_tickets_and_second_admission() {
    use crate::library::enrichment::{LinkCorrection, LinkKind};
    let f = setup(true).await;
    let path = "/api/v1/recordings/1/files/1/playback-ticket";
    let (_, _, ticket) =
        request(&f, "POST", path, Some(&f.member), None).await;
    let intent = db::removals::start(&f.pool, 1, 1, 1, false)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db::removals::start(&f.pool, 1, 1, 1, false)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        db::metadata::relink(
            &f.pool,
            LinkKind::File,
            1,
            LinkCorrection {
                expected_recording_id: 1,
                recording_id: 2
            },
            1
        )
        .await,
        Err(db::metadata::MetadataError::Conflict)
    ));
    assert_eq!(
        request(&f, "POST", path, Some(&f.member), None).await.0,
        404
    );
    assert_eq!(
        request(
            &f,
            "GET",
            ticket["stream_url"].as_str().unwrap(),
            None,
            None
        )
        .await
        .0,
        401
    );
    let (_, _, card) =
        request(&f, "GET", "/api/v1/recordings/1", None, None).await;
    assert_eq!(card["files"], json!([]));
    assert_eq!(card["has_audio"], false);
    db::removals::complete(&f.pool, intent.id).await.unwrap();
    f.pool.close().await;
}
