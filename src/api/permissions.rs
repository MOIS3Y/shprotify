//! Authorization regression tests for all shared administrative operations.

use crate::{
    auth::{self, Role},
    db,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Send a request with a deliberately untrusted role header and optional key.
async fn request(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    value: Value,
) -> (u16, HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-role", "admin")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder =
            builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, headers, body)
}

/// Every administrative route rejects anonymous/member requests before writes.
#[tokio::test]
async fn enforces_the_complete_shared_operation_policy() {
    let directory = tempfile::tempdir().unwrap();
    let pool = db::connect(&directory.path().join("catalog.db"))
        .await
        .unwrap();
    let token = auth::token().unwrap();
    db::users::create(&pool, "member", &auth::digest(&token))
        .await
        .unwrap();
    sqlx::query("INSERT INTO recordings (id, title) VALUES (1, 'Existing')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO releases (id, title, kind) \
        VALUES (1, 'Existing', 'other')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let service = crate::jobs::ScanService::new(pool.clone(), vec![]);
    let app = super::router(
        &crate::config::DocsConfig { enabled: true },
        &pool,
        service.clone(),
    );
    let operations = [
        ("POST", "/api/v1/artists"),
        ("POST", "/api/v1/releases"),
        ("POST", "/api/v1/recordings"),
        ("POST", "/api/v1/releases/1/tracks"),
        ("POST", "/api/v1/recordings/1/files"),
        ("POST", "/api/v1/library/scans"),
        ("GET", "/api/v1/library/scans"),
        ("GET", "/api/v1/library/scans/1"),
        ("GET", "/api/v1/library/scans/1/errors"),
        ("GET", "/api/v1/library/duplicates"),
        ("HEAD", "/api/v1/library/scans/1/errors"),
    ];
    for (key, expected) in [(None, 401), (Some(token.as_str()), 403)] {
        for (method, path) in operations {
            let (status, headers, body) =
                request(&app, method, path, key, json!({})).await;
            assert_eq!(status, expected, "{method} {path}");
            assert_eq!(
                headers.contains_key(header::WWW_AUTHENTICATE),
                status == 401
            );
            if method != "HEAD" {
                assert_eq!(
                    body["error"]["code"],
                    if status == 401 {
                        "unauthorized"
                    } else {
                        "forbidden"
                    }
                );
            }
        }
    }
    let counts: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM artists), \
         (SELECT COUNT(*) FROM releases), \
         (SELECT COUNT(*) FROM recordings), \
         (SELECT COUNT(*) FROM release_tracks), \
         (SELECT COUNT(*) FROM audio_files), \
         (SELECT COUNT(*) FROM library_scans)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (0, 1, 1, 0, 0, 0));
    for path in [
        "/api/v1/artists",
        "/api/v1/releases",
        "/api/v1/recordings",
        "/api/v1/search?q=Existing",
        "/api/v1/health",
    ] {
        assert_eq!(request(&app, "GET", path, None, Value::Null).await.0, 200);
    }
    assert_eq!(
        request(
            &app,
            "PUT",
            "/api/v1/me/recordings/1/reaction",
            Some(&token),
            json!({"reaction":"like"})
        )
        .await
        .0,
        200
    );
    let (_, _, favorites) = request(
        &app,
        "GET",
        "/api/v1/me/favorites",
        Some(&token),
        Value::Null,
    )
    .await;
    assert_eq!(favorites["items"].as_array().unwrap().len(), 1);
    db::users::set_role(&pool, "member", Role::Admin)
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/v1/artists",
            Some(&token),
            json!({"name":"Created"})
        )
        .await
        .0,
        201
    );
    let (status, _, job) = request(
        &app,
        "POST",
        "/api/v1/library/scans",
        Some(&token),
        Value::Null,
    )
    .await;
    assert_eq!(status, 202);
    service.close().await;
    assert_eq!(
        request(
            &app,
            "GET",
            &format!("/api/v1/library/scans/{}", job["id"]),
            Some(&token),
            Value::Null
        )
        .await
        .0,
        200
    );
    assert_eq!(
        request(
            &app,
            "GET",
            "/api/v1/library/duplicates",
            Some(&token),
            Value::Null
        )
        .await
        .0,
        200
    );
    db::users::set_role(&pool, "member", Role::Member)
        .await
        .unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/v1/artists",
            Some(&token),
            json!({"name":"Forbidden"})
        )
        .await
        .0,
        403
    );
    let (_, _, me) =
        request(&app, "GET", "/api/v1/me", Some(&token), Value::Null).await;
    assert_eq!(me["role"], "member");
    db::users::set_role(&pool, "member", Role::Admin)
        .await
        .unwrap();
    db::users::revoke(&pool, "member").await.unwrap();
    assert_eq!(
        request(
            &app,
            "POST",
            "/api/v1/artists",
            Some(&token),
            json!({"name":"Revoked"})
        )
        .await
        .0,
        401
    );
    pool.close().await;
}

/// Swagger describes all current role-protected operations and public reads.
#[tokio::test]
async fn schema_documents_every_administrative_operation() {
    let pool = sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap();
    let app = super::router(
        &crate::config::DocsConfig { enabled: true },
        &pool,
        crate::jobs::ScanService::new(pool.clone(), vec![]),
    );
    let (_, _, schema) =
        request(&app, "GET", "/openapi.json", None, Value::Null).await;
    let mut count = 0;
    for (path, operations) in schema["paths"].as_object().unwrap() {
        for method in ["get", "post", "put", "patch", "delete"] {
            let operation = &operations[method];
            if operation.is_null() {
                continue;
            }
            let catalog =
                ["/api/v1/artists", "/api/v1/releases", "/api/v1/recordings"]
                    .iter()
                    .any(|prefix| path.starts_with(prefix));
            if (method != "get" && catalog)
                || path.starts_with("/api/v1/library/scans")
                || path == "/api/v1/library/duplicates"
            {
                count += 1;
                assert!(operation["security"][0]["personal_key"].is_array());
                assert!(operation["responses"]["401"].is_object());
                assert!(operation["responses"]["403"].is_object());
            }
        }
    }
    assert_eq!(count, 10);
    assert!(schema["paths"]["/api/v1/artists"]["get"]["security"].is_null());
    assert_eq!(
        schema["components"]["schemas"]["Role"]["enum"],
        json!(["member", "admin"])
    );
}
