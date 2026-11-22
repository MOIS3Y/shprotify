//! Personal, client-reported listening history with retry-safe submissions.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    audio::history::{NewPlayback, PlaybackEvent},
    auth::CurrentUser,
    db,
};
use axum::{
    Json,
    extract::{
        DefaultBodyLimit, OriginalUri, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Bind account-scoped history routes to the catalog database.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(report_playback, list_history))
        .routes(routes!(get_history_item))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(pool)
}

/// Final facts for one playback; generate a new event ID for each replay.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ReportPlayback {
    /// Case-sensitive idempotency key; reuse only for identical retries.
    #[schema(min_length = 1, max_length = 128, pattern = "^[A-Za-z0-9._-]+$")]
    event_id: String,
    /// Shared catalog recording identity, independent of file encoding.
    #[schema(minimum = 1)]
    recording_id: i64,
    /// Playback start in Unix seconds; maximum five minutes ahead of server.
    #[schema(minimum = 0)]
    started_at: i64,
    /// Played milliseconds without pauses/seeks; cap at known duration.
    #[schema(minimum = 1)]
    listened_ms: i64,
    /// Known duration in milliseconds; omitted/null if unavailable.
    #[schema(minimum = 1)]
    duration_ms: Option<i64>,
}

/// A bounded page of the authenticated account's listening history.
#[derive(Serialize, ToSchema)]
struct HistoryPage {
    /// Newest playback starts first, with descending ID for equal timestamps.
    items: Vec<PlaybackEvent>,
    /// Maximum events returned.
    limit: i64,
    /// Number of events skipped.
    offset: i64,
}

/// Report one stopped/finished playback, never an HTTP download or seek.
/// Identical account/event retries return 200 with the original item.
/// Full means half the known duration (rounded up) or four minutes, whichever
/// is shorter; unknown duration uses four minutes. Other events are short.
#[utoipa::path(post, path = "/me/history", tag = "History",
    operation_id = "report_playback", request_body = ReportPlayback,
    security(("personal_key" = [])), responses(
        (status = 201, body = PlaybackEvent, description = "Playback recorded",
            headers(("Location" = String, description = "History URL"))),
        (status = 200, body = PlaybackEvent, description = "Identical retry"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn report_playback(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    OriginalUri(uri): OriginalUri,
    input: Result<Json<ReportPlayback>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = input?;
    let input = NewPlayback::new(
        input.event_id,
        input.recording_id,
        input.started_at,
        input.listened_ms,
        input.duration_ms,
    )
    .map_err(ApiError::Validation)?;
    let saved = db::history::record(&pool, user.id, input).await
        .map_err(|error| match error {
            db::history::HistoryError::MissingRecording =>
                ApiError::Validation("Recording does not exist"),
            db::history::HistoryError::FutureTimestamp => ApiError::Validation(
                "started_at must not be more than five minutes in the future"
            ),
            db::history::HistoryError::Conflict => ApiError::Conflict(
                "event_id already identifies a different playback report"
            ),
            db::history::HistoryError::Storage(error) => error.into(),
        })?;
    let status = if saved.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(private_response((
        status,
        [(
            header::LOCATION,
            format!("{}/{}", uri.path(), saved.event.id),
        )],
        Json(saved.event),
    )))
}

/// Read only this account's history, ordered by reported playback start.
#[utoipa::path(get, path = "/me/history", tag = "History",
    operation_id = "list_history", params(Pagination),
    security(("personal_key" = [])), responses(
        (status = 200, body = HistoryPage, description = "History page"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn list_history(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(page) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    page.validate()?;
    let items =
        db::history::list(&pool, user.id, page.limit, page.offset).await?;
    Ok(private_response(Json(HistoryPage {
        items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// Read an owned event; another account's identifier receives the same 404.
#[utoipa::path(get, path = "/me/history/{id}", tag = "History",
    operation_id = "get_history_item",
    params(("id" = i64, Path, minimum = 1, description = "History event ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = PlaybackEvent, description = "Private event"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_history_item(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid history ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("History ID must be positive"));
    }
    let event = db::history::find(&pool, user.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(private_response(Json(event)))
}

/// Prevent history representations from entering browser or shared caches.
fn private_response(value: impl IntoResponse) -> Response {
    let mut response = value.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    //! Full HTTP contracts with real account credentials and isolated SQLite.

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
        let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, headers, body)
    }

    /// A stopped playback reporting a full half of a three-minute recording.
    fn report(key: &str) -> Value {
        json!({"event_id":key,"recording_id":1,"started_at":1000,
            "listened_ms":90_000,"duration_ms":180_000})
    }

    /// Test account identities, without any production authentication bypass.
    struct Fixture {
        /// Retain the file-backed SQLite directory until the test finishes.
        _directory: tempfile::TempDir,
        /// Shared catalog and personal activity database.
        pool: SqlitePool,
        /// Complete API with generated documentation enabled.
        app: Router,
        /// Ordinary member who reports the first playback.
        alice: String,
        /// Another account with independent history and idempotency keys.
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

    /// Replays, retries and private IDs behave consistently for every account.
    #[tokio::test]
    async fn reports_retry_safe_events_and_keeps_history_private() {
        let fixture = Fixture::new().await;
        let url = "/api/v1/me/history";
        let (_, headers, empty) =
            request(&fixture.app, "GET", url, Some(&fixture.alice), None)
                .await;
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(empty["items"], json!([]));
        let (status, headers, event) = request(
            &fixture.app,
            "POST",
            url,
            Some(&fixture.alice),
            Some(report("phone-play-1")),
        )
        .await;
        assert_eq!(status, 201);
        assert_eq!(event["kind"], "full");
        assert_eq!(event["title"], "Track");
        assert_eq!(event["recording_id"], 1);
        assert!(event.get("user_id").is_none());
        assert!(event["received_at"].as_str().unwrap().ends_with('Z'));
        let location = headers[header::LOCATION].to_str().unwrap();
        assert_eq!(location, "/api/v1/me/history/1");
        let retry = request(
            &fixture.app,
            "POST",
            url,
            Some(&fixture.alice),
            Some(report("phone-play-1")),
        )
        .await;
        assert_eq!(retry.0, 200);
        assert_eq!(retry.2, event);
        let mut changed = report("phone-play-1");
        changed["listened_ms"] = json!(1);
        let conflict = request(
            &fixture.app,
            "POST",
            url,
            Some(&fixture.alice),
            Some(changed),
        )
        .await;
        assert_eq!(conflict.0, 409);
        assert_eq!(conflict.2["error"]["code"], "conflict");
        assert_eq!(
            request(&fixture.app, "GET", location, Some(&fixture.alice), None)
                .await
                .2,
            event
        );
        db::users::set_role(&fixture.pool, "bob", crate::auth::Role::Admin)
            .await
            .unwrap();
        for path in [location, "/api/v1/me/history/999"] {
            assert_eq!(
                request(&fixture.app, "GET", path, Some(&fixture.bob), None)
                    .await
                    .0,
                404
            );
        }
        assert_eq!(
            request(
                &fixture.app,
                "POST",
                url,
                Some(&fixture.bob),
                Some(report("phone-play-1"))
            )
            .await
            .0,
            201
        );
        let mut short = report("phone-play-2");
        short["listened_ms"] = json!(5000);
        short["started_at"] = json!(500);
        assert_eq!(
            request(
                &fixture.app,
                "POST",
                url,
                Some(&fixture.alice),
                Some(short)
            )
            .await
            .2["kind"],
            "short"
        );
        let page = request(
            &fixture.app,
            "GET",
            "/api/v1/me/history?limit=1",
            Some(&fixture.alice),
            None,
        )
        .await
        .2;
        assert_eq!(page["items"], json!([event]));
        let page = request(
            &fixture.app,
            "GET",
            "/api/v1/me/history?limit=1&offset=1",
            Some(&fixture.alice),
            None,
        )
        .await
        .2;
        assert_eq!(page["items"][0]["event_id"], "phone-play-2");
        assert_eq!(page["items"][0]["kind"], "short");
        let mut unknown = report("phone-play-3");
        unknown.as_object_mut().unwrap().remove("duration_ms");
        unknown["listened_ms"] = json!(240_000);
        let unknown = request(
            &fixture.app,
            "POST",
            url,
            Some(&fixture.alice),
            Some(unknown),
        )
        .await;
        assert_eq!(unknown.0, 201);
        assert_eq!(unknown.2["kind"], "full");
        assert!(unknown.2["duration_ms"].is_null());
        let favorites = request(
            &fixture.app,
            "GET",
            "/api/v1/me/favorites",
            Some(&fixture.alice),
            None,
        )
        .await
        .2;
        assert_eq!(favorites["items"], json!([]));
        db::users::revoke(&fixture.pool, "alice").await.unwrap();
        assert_eq!(
            request(&fixture.app, "GET", url, Some(&fixture.alice), None)
                .await
                .0,
            401
        );
        assert_eq!(
            db::history::list(&fixture.pool, 1, 50, 0)
                .await
                .unwrap()
                .len(),
            3
        );
        fixture.pool.close().await;
    }

    /// Reject malformed or spoofed facts before inserting any history row.
    #[tokio::test]
    async fn authenticates_and_validates_reports_without_side_effects() {
        let fixture = Fixture::new().await;
        let url = "/api/v1/me/history";
        for (method, path) in [
            ("POST", url),
            ("GET", url),
            ("HEAD", "/api/v1/me/history/x"),
            ("GET", "/api/v1/me/history/x"),
        ] {
            let (status, headers, _) =
                request(&fixture.app, method, path, None, Some(json!({})))
                    .await;
            assert_eq!(status, 401);
            assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
        }
        for (field, value) in [
            ("event_id", json!("")),
            ("event_id", json!("../private")),
            ("event_id", json!("a".repeat(129))),
            ("recording_id", json!(0)),
            ("recording_id", json!(999)),
            ("started_at", json!(-1)),
            ("started_at", json!(i64::MAX)),
            ("listened_ms", json!(0)),
            ("listened_ms", json!(180_001)),
            ("duration_ms", json!(0)),
            ("duration_ms", json!(10)),
            ("user_id", json!(2)),
            ("kind", json!("full")),
        ] {
            let mut invalid = report("bad");
            invalid[field] = value;
            assert_eq!(
                request(
                    &fixture.app,
                    "POST",
                    url,
                    Some(&fixture.alice),
                    Some(invalid)
                )
                .await
                .0,
                422,
                "{field}"
            );
        }
        for field in ["event_id", "recording_id", "started_at", "listened_ms"]
        {
            let mut invalid = report("missing");
            invalid.as_object_mut().unwrap().remove(field);
            assert_eq!(
                request(
                    &fixture.app,
                    "POST",
                    url,
                    Some(&fixture.alice),
                    Some(invalid)
                )
                .await
                .0,
                422
            );
        }
        assert_eq!(
            request(&fixture.app, "POST", url, Some(&fixture.alice), None)
                .await
                .0,
            415
        );
        let mut large = report("large");
        large["event_id"] = json!("a".repeat(5000));
        assert_eq!(
            request(
                &fixture.app,
                "POST",
                url,
                Some(&fixture.alice),
                Some(large)
            )
            .await
            .0,
            413
        );
        for query in ["limit=0", "limit=101", "offset=-1", "user_id=2"] {
            assert_eq!(
                request(
                    &fixture.app,
                    "GET",
                    &format!("{url}?{query}"),
                    Some(&fixture.alice),
                    None
                )
                .await
                .0,
                400
            );
        }
        for id in ["0", "-1", "invalid"] {
            assert_eq!(
                request(
                    &fixture.app,
                    "GET",
                    &format!("{url}/{id}"),
                    Some(&fixture.alice),
                    None
                )
                .await
                .0,
                400
            );
        }
        assert!(
            db::history::list(&fixture.pool, 1, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        fixture.pool.close().await;
    }

    /// Generated OpenAPI describes immutable facts and private report routes.
    #[tokio::test]
    async fn documents_history_security_schema_and_retry_statuses() {
        let fixture = Fixture::new().await;
        let schema = request(&fixture.app, "GET", "/openapi.json", None, None)
            .await
            .2;
        for (path, method) in [
            ("/api/v1/me/history", "post"),
            ("/api/v1/me/history", "get"),
            ("/api/v1/me/history/{id}", "get"),
        ] {
            let operation = &schema["paths"][path][method];
            assert!(operation["security"][0]["personal_key"].is_array());
            assert!(operation["responses"]["401"].is_object());
        }
        let report = &schema["paths"]["/api/v1/me/history"]["post"];
        assert!(report["responses"]["201"]["headers"]["Location"].is_object());
        assert!(report["responses"]["200"].is_object());
        assert!(report["responses"]["409"].is_object());
        let input = &schema["components"]["schemas"]["ReportPlayback"];
        assert_eq!(input["properties"]["event_id"]["maxLength"], 128);
        assert!(input["properties"].get("kind").is_none());
        assert_eq!(
            schema["components"]["schemas"]["PlaybackKind"]["enum"],
            json!(["full", "short"])
        );
        fixture.pool.close().await;
    }
}
