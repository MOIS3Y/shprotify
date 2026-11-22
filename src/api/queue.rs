//! Authenticated personal queue and explicit client playback position.

use super::error::{ApiError, ErrorResponse};
use crate::{
    audio::queue::{MAX_TRACKS, Queue},
    auth::CurrentUser,
    db,
};
use axum::{
    Json,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Mount private queue routes; account identity always comes from credentials.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(get_queue, replace_queue))
        .routes(routes!(set_playback))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(pool)
}

/// Replace the full order and selection atomically, including repeated IDs.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ReplaceQueue {
    /// Last observed version; initial empty queue uses zero.
    #[schema(minimum = 0)]
    version: i64,
    /// Ordered occurrences; empty clears the queue.
    #[schema(max_items = 10000)]
    recording_ids: Vec<i64>,
    /// One-based occurrence in the replacement order; omission/null deselects.
    #[schema(minimum = 1)]
    current_position: Option<i64>,
    /// Media offset in milliseconds; zero when no occurrence is selected.
    #[schema(minimum = 0)]
    position_ms: i64,
}

/// Change selection/offset without resubmitting the order.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct SetPlayback {
    /// Same version as the queue order; stale updates receive 409.
    #[schema(minimum = 0)]
    version: i64,
    /// One-based occurrence; omission/null clears the selection.
    #[schema(minimum = 1)]
    current_position: Option<i64>,
    /// Media offset; no duration or listening-time inference is made.
    #[schema(minimum = 0)]
    position_ms: i64,
}

/// Read the complete bounded order and playback position from one snapshot.
#[utoipa::path(get, path = "/me/queue", tag = "queue",
    operation_id = "get_queue", security(("personal_key" = [])), responses(
        (status = 200, body = Queue, description = "Personal queue snapshot"),
        (status = 401, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_queue(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
) -> Result<Response, ApiError> {
    Ok(private(db::queue::get(&pool, user.id).await?))
}

/// Replace the order and explicitly select an occurrence in the new order.
/// Duplicate recordings are allowed; no-op requests preserve the version.
#[utoipa::path(put, path = "/me/queue", tag = "queue",
    operation_id = "replace_queue", request_body = ReplaceQueue,
    security(("personal_key" = [])), responses(
        (status = 200, body = Queue, description = "Updated queue snapshot"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn replace_queue(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    input: Result<Json<ReplaceQueue>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = input?;
    if input.recording_ids.len() > MAX_TRACKS
        || input.recording_ids.iter().any(|id| *id <= 0)
    {
        return Err(ApiError::Validation(
            "recording_ids must contain at most 10000 positive IDs",
        ));
    }
    save(
        &pool,
        user.id,
        input.version,
        Some(&input.recording_ids),
        input.current_position,
        input.position_ms,
    )
    .await
}

/// Persist a seek or selection; the client decides when to advance tracks.
/// This does not create history, issue playback commands, or open audio files.
#[utoipa::path(put, path = "/me/queue/playback", tag = "queue",
    operation_id = "set_queue_playback", request_body = SetPlayback,
    security(("personal_key" = [])), responses(
        (status = 200, body = Queue, description = "Updated queue snapshot"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn set_playback(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    input: Result<Json<SetPlayback>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = input?;
    save(
        &pool,
        user.id,
        input.version,
        None,
        input.current_position,
        input.position_ms,
    )
    .await
}

/// Map storage failures without exposing diagnostics or accepting a user ID.
async fn save(
    pool: &SqlitePool,
    user: i64,
    version: i64,
    recordings: Option<&[i64]>,
    current: Option<i64>,
    position_ms: i64,
) -> Result<Response, ApiError> {
    if version < 0 {
        return Err(ApiError::Validation("version must be nonnegative"));
    }
    let queue = db::queue::update(
        pool,
        user,
        version,
        recordings,
        current,
        position_ms,
    )
    .await
    .map_err(|error| match error {
        db::queue::QueueError::StaleVersion => {
            ApiError::Conflict("Queue version changed; reload before retrying")
        }
        db::queue::QueueError::MissingRecording => {
            ApiError::Validation("One or more recordings do not exist")
        }
        db::queue::QueueError::InvalidPosition(message) => {
            ApiError::Validation(message)
        }
        db::queue::QueueError::Storage(error) => error.into(),
    })?;
    Ok(private(queue))
}

/// Keep personal snapshots out of browser and shared caches.
fn private(queue: Queue) -> Response {
    let mut response = Json(queue).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    //! HTTP privacy, validation and generated documentation contracts.

    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{HeaderMap, Request},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Send a JSON request with optional credentials through the real router.
    async fn request(
        app: &Router,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some(body) => {
                builder =
                    builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, headers, body)
    }

    /// Test account identities, without any production authentication bypass.
    struct Fixture {
        /// Retain the file-backed SQLite directory until the test finishes.
        _directory: tempfile::TempDir,
        /// Shared catalog and personal activity database.
        pool: SqlitePool,
        /// Complete API with generated documentation enabled.
        app: Router,
        /// Ordinary member who owns the first queue.
        alice: String,
        /// Another account with independent queue.
        bob: String,
    }

    impl Fixture {
        /// Seed two accounts and one recording with no physical audio files.
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let pool = db::connect(&directory.path().join("catalog.db"))
                .await
                .unwrap();
            let alice = crate::auth::token().unwrap();
            let bob = crate::auth::token().unwrap();
            for (name, key) in [("alice", &alice), ("bob", &bob)] {
                db::users::create(&pool, name, &crate::auth::digest(key))
                    .await
                    .unwrap();
            }
            sqlx::query(
                "INSERT INTO recordings (id, title) VALUES (1,'Track')",
            )
            .execute(&pool)
            .await
            .unwrap();
            let app = crate::api::router(
                &crate::config::AuthConfig::default(),
                &crate::config::DocsConfig { enabled: true },
                &pool,
                crate::jobs::ScanService::new(pool.clone(), vec![]),
            );
            Self {
                _directory: directory,
                pool,
                app,
                alice,
                bob,
            }
        }
    }

    /// Private queue works with real keys/sessions; roles do not share state.
    #[tokio::test]
    async fn manages_private_queue_without_creating_history() {
        let f = Fixture::new().await;
        let url = "/api/v1/me/queue";
        let (status, headers, empty) =
            request(&f.app, "GET", url, Some(&f.alice), None).await;
        assert_eq!(status, 200);
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(
            empty,
            json!({"version":0,"current_position":null,
            "position_ms":0,"items":[]})
        );
        let input = json!({"version":0,"recording_ids":[1,1],
            "current_position":2,"position_ms":50});
        let saved =
            request(&f.app, "PUT", url, Some(&f.alice), Some(input.clone()))
                .await;
        assert_eq!(saved.0, 200);
        assert_eq!(saved.2["version"], 1);
        assert_eq!(saved.2["items"][1]["position"], 2);
        assert_eq!(saved.2["current_position"], 2);
        assert_eq!(
            request(&f.app, "PUT", url, Some(&f.alice), Some(input))
                .await
                .0,
            409
        );
        db::users::set_role(&f.pool, "bob", crate::auth::Role::Admin)
            .await
            .unwrap();
        assert_eq!(
            request(&f.app, "GET", url, Some(&f.bob), None).await.2,
            empty
        );
        assert_eq!(
            request(&f.app, "GET", "/api/v1/me/queue?user_id=1", None, None)
                .await
                .0,
            401
        );
        let alice = db::users::find(&f.pool, 1).await.unwrap().unwrap();
        let token = crate::auth::session_token().unwrap();
        // Persist a session directly to avoid unrelated password hash work.
        sqlx::query(
            "INSERT INTO user_sessions (user_id, digest, expires_at) \
            VALUES (?, ?, unixepoch() + 3600)",
        )
        .bind(alice.id)
        .bind(crate::auth::digest(&token))
        .execute(&f.pool)
        .await
        .unwrap();
        let moved = request(
            &f.app,
            "PUT",
            "/api/v1/me/queue/playback",
            Some(&token),
            Some(json!({"version":1,
                "current_position":1,"position_ms":1000})),
        )
        .await;
        assert_eq!(moved.0, 200);
        assert_eq!(moved.2["version"], 2);
        assert_eq!(moved.2["items"], saved.2["items"]);
        assert_eq!(
            request(&f.app, "GET", url, Some(&f.alice), None).await.2,
            moved.2
        );
        let cleared = request(
            &f.app,
            "PUT",
            url,
            Some(&f.alice),
            Some(json!({"version":2,"recording_ids":[],
                "position_ms":0})),
        )
        .await;
        assert_eq!(cleared.0, 200);
        assert_eq!(cleared.2["version"], 3);
        assert_eq!(cleared.2["items"], json!([]));
        for query in [
            "SELECT count(*) FROM playback_events",
            "SELECT count(*) FROM playlists",
            "SELECT count(*) FROM recording_reactions",
        ] {
            let count: i64 =
                sqlx::query_scalar(query).fetch_one(&f.pool).await.unwrap();
            assert_eq!(count, 0);
        }
        db::users::revoke(&f.pool, "alice").await.unwrap();
        assert_eq!(
            request(&f.app, "GET", url, Some(&f.alice), None).await.0,
            401
        );
        f.pool.close().await;
    }

    /// The maximum order is served whole without a SQL bind per occurrence.
    #[tokio::test]
    async fn serves_maximum_order_and_head_without_truncation() {
        let f = Fixture::new().await;
        let url = "/api/v1/me/queue";
        let saved = request(
            &f.app,
            "PUT",
            url,
            Some(&f.alice),
            Some(json!({"version":0,"recording_ids":vec![1;10000],
                "current_position":10000,"position_ms":0})),
        )
        .await;
        assert_eq!(saved.0, 200);
        assert_eq!(saved.2["items"].as_array().unwrap().len(), 10000);
        let read = request(&f.app, "GET", url, Some(&f.alice), None).await;
        assert_eq!(read.2, saved.2);
        let head = request(&f.app, "HEAD", url, Some(&f.alice), None).await;
        assert_eq!(head.0, 200);
        assert!(head.2.is_null());
        assert_eq!(head.1[header::CACHE_CONTROL], "private, no-store");
        f.pool.close().await;
    }

    /// Authentication precedes invalid bodies; rejected input preserves state.
    #[tokio::test]
    async fn rejects_invalid_bodies_and_documents_all_operations() {
        let f = Fixture::new().await;
        let url = "/api/v1/me/queue";
        for path in [url, "/api/v1/me/queue/playback"] {
            for method in ["GET", "PUT"] {
                if method == "GET" && path.ends_with("playback") {
                    continue;
                }
                let (status, headers, _) = request(
                    &f.app,
                    method,
                    path,
                    None,
                    Some(json!({"wrong":true})),
                )
                .await;
                assert_eq!(status, 401);
                assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
            }
        }
        for body in [
            json!({"version":-1,"recording_ids":[],"position_ms":0}),
            json!({"version":0,"recording_ids":[0],"position_ms":0}),
            json!({"version":0,"recording_ids":[999],"position_ms":0}),
            json!({"version":0,"recording_ids":[],"position_ms":1}),
            json!({"version":0,"recording_ids":[],"position_ms":-1}),
            json!({"version":0,"recording_ids":[1],
                "current_position":2,"position_ms":0}),
            json!({"version":0,"recording_ids":[1],
                "current_position":0,"position_ms":0}),
            json!({"version":0,"recording_ids":[],"position_ms":0,
                "user_id":2}),
            json!({"version":0,"recording_ids":[],"position_ms":null}),
            json!({"recording_ids":[],"position_ms":0}),
            json!({"version":0,"recording_ids":vec![1;10001],
                "position_ms":0}),
        ] {
            let result = request(
                &f.app,
                "PUT",
                url,
                Some(&f.alice),
                Some(body.clone()),
            )
            .await;
            assert_eq!(result.0, 422, "{body}");
        }
        assert_eq!(
            request(&f.app, "PUT", url, Some(&f.alice), None).await.0,
            415
        );
        let malformed = Request::builder()
            .method("PUT")
            .uri(url)
            .header(header::AUTHORIZATION, format!("Bearer {}", f.alice))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap();
        assert_eq!(
            f.app
                .clone()
                .oneshot(malformed)
                .await
                .unwrap()
                .status()
                .as_u16(),
            400
        );
        let oversized = Request::builder()
            .method("PUT")
            .uri(url)
            .header(header::AUTHORIZATION, format!("Bearer {}", f.alice))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(" ".repeat(256 * 1024 + 1)))
            .unwrap();
        assert_eq!(
            f.app
                .clone()
                .oneshot(oversized)
                .await
                .unwrap()
                .status()
                .as_u16(),
            413
        );
        let initial = request(&f.app, "GET", url, Some(&f.alice), None).await;
        assert_eq!(initial.2["version"], 0);
        let schema =
            request(&f.app, "GET", "/openapi.json", None, None).await.2;
        for (path, methods) in [
            (url, vec!["get", "put"]),
            ("/api/v1/me/queue/playback", vec!["put"]),
        ] {
            for method in methods {
                assert_eq!(
                    schema["paths"][path][method]["security"],
                    json!([{"personal_key":[]}])
                );
                assert!(
                    schema["paths"][path][method]["responses"]["200"]
                        .is_object()
                );
            }
        }
        f.pool.close().await;
    }
}
