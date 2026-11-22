//! Recording HTTP contracts, validation, and atomic catalog operations.

use axum::{
    Json,
    extract::{
        OriginalUri, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    db,
    library::recordings::{NewRecording, Recording},
};

use crate::library::browse::{RecordingQuery, RecordingSummary};

use super::error::{ApiError, ErrorResponse};

/// Register version-local recording operations with the shared SQLite pool.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_recording, list_recordings))
        .routes(routes!(get_recording))
        .routes(routes!(artist_recordings))
        .routes(routes!(batch_recordings))
        .with_state(pool)
}

/// Recording input; artist credits preserve caller-supplied order.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CreateRecording {
    /// Title trimmed before validating 1–255 characters without controls.
    #[schema(example = "Teardrop")]
    title: String,
    /// Up to 100 distinct positive existing IDs; omitted means no credits.
    #[serde(default)]
    #[schema(max_items = 100)]
    artist_ids: Vec<i64>,
}

/// Bounded catalog slice; identifier order is the default.
#[derive(Serialize, ToSchema)]
struct RecordingPage {
    /// Recordings with complete ordered credits; empty beyond the last page.
    items: Vec<RecordingSummary>,
    /// Effective maximum number of recordings in this response.
    limit: i64,
    /// Total rows matching this filter in the same read snapshot.
    total: i64,
    /// Whether another page exists after the returned items.
    has_more: bool,
    /// Number of skipped recordings.
    offset: i64,
}

/// Create a recording and its ordered artist credits atomically.
///
/// A recording requires neither a release track position nor an audio file.
///
/// Titles may repeat and credits may be omitted. A nonexistent credited artist
/// causes 422 and leaves no recording or credit rows behind.
///
/// Requires the administrator role.
#[utoipa::path(
    post, path = "/recordings", tag = "Admin: Catalog",
    operation_id = "create_recording", request_body = CreateRecording,
    security(("personal_key" = [])),
    responses(
        (status = 201, body = Recording, description = "Recording created",
            headers(("Location" = String, description = "Recording URL"))),
        (status = 400, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn create_recording(
    _admin: crate::auth::Admin,
    State(pool): State<SqlitePool>,
    OriginalUri(uri): OriginalUri,
    payload: Result<Json<CreateRecording>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(payload) = payload.map_err(ApiError::from)?;
    let input = NewRecording::new(&payload.title, payload.artist_ids)
        .map_err(ApiError::Validation)?;
    let recording = db::recordings::create(&pool, input).await.map_err(
        |error| match error {
            db::recordings::CreateRecordingError::UnknownArtist => {
                ApiError::Validation("A credited artist does not exist")
            }
            db::recordings::CreateRecordingError::Storage(error) => {
                error.into()
            }
        },
    )?;
    let location = format!("{}/{}", uri.path(), recording.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(recording),
    )
        .into_response())
}

/// Retrieve one recording with credits in their original order.
#[utoipa::path(
    get, path = "/recordings/{id}", tag = "Recordings",
    operation_id = "get_recording",
    params(("id" = i64, Path, minimum = 1, description = "Recording ID")),
    responses(
        (status = 200, body = RecordingSummary,
            description = "Recording found"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn get_recording(
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<RecordingSummary>, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid recording id"))?;
    Ok(Json(
        db::browse::recording(&pool, id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// Filter recordings with complete display credits and path-free audio data.
#[utoipa::path(
    get, path = "/recordings", tag = "Recordings",
    operation_id = "list_recordings",
    params(RecordingQuery),
    responses(
        (status = 200, body = RecordingPage, description = "Recording page"),
        (status = 400, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn list_recordings(
    State(pool): State<SqlitePool>,
    query: Result<Query<RecordingQuery>, QueryRejection>,
) -> Result<Json<RecordingPage>, ApiError> {
    let Query(query) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    query.validate().map_err(ApiError::BadRequest)?;
    let (items, total) = db::browse::recordings(&pool, &query).await?;
    Ok(Json(RecordingPage {
        has_more: query.offset.saturating_add(items.len() as i64) < total,
        total,
        items,
        limit: query.limit,
        offset: query.offset,
    }))
}

/// Browse this artist's recordings; a conflicting query scope is rejected.
#[utoipa::path(get, path="/artists/{id}/recordings", tag="Artists",
    operation_id="artist_recordings",
    params(("id"=i64,Path,minimum=1),RecordingQuery),
    responses((status=200,description="Filtered catalog page",
        body=RecordingPage),
        (status=400,response=ErrorResponse),
        (status=401,response=ErrorResponse),
        (status=404,response=ErrorResponse),
        (status=500,response=ErrorResponse)))]
async fn artist_recordings(
    State(pool): State<SqlitePool>,
    path: Result<Path<i64>, PathRejection>,
    query: Result<Query<RecordingQuery>, QueryRejection>,
) -> Result<Json<RecordingPage>, ApiError> {
    let Path(id) =
        path.map_err(|_| ApiError::BadRequest("Invalid parent ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("ID must be positive"));
    }
    if db::artists::find(&pool, id).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    let Query(mut query) =
        query.map_err(|_| ApiError::BadRequest("Invalid catalog query"))?;
    if query.artist_id.is_some_and(|value| value != id) {
        return Err(ApiError::BadRequest(
            "Query scope conflicts with parent ID",
        ));
    }
    query.artist_id = Some(id);
    query.validate().map_err(ApiError::BadRequest)?;
    let (items, total) = db::browse::recordings(&pool, &query).await?;
    Ok(Json(RecordingPage {
        has_more: query.offset.saturating_add(items.len() as i64) < total,
        items,
        total,
        limit: query.limit,
        offset: query.offset,
    }))
}

/// Bounded bulk lookup for queue, history, favorites and playlist rendering.
#[derive(Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
struct BatchQuery {
    /// Comma-separated positive recording IDs, at most 100 distinct values.
    ids: String,
}

/// Existing cards keep input order; missing references are explicit.
#[derive(Serialize, ToSchema)]
struct RecordingBatch {
    /// Full display/audio summaries; no request per track is needed.
    items: Vec<RecordingSummary>,
    /// Requested IDs absent from the current catalog.
    missing_ids: Vec<i64>,
}

/// Resolve one client page of recording IDs in a single request.
/// Empty `ids` is accepted. Duplicates or more than 100 IDs return 400.
#[utoipa::path(get,path="/recordings/batch",tag="Recordings",
    params(BatchQuery),responses(
        (status=200,body=RecordingBatch,description="Recording cards"),
        (status=400,response=ErrorResponse),
        (status=401,response=ErrorResponse),
        (status=500,response=ErrorResponse)
    ))]
async fn batch_recordings(
    State(pool): State<SqlitePool>,
    query: Result<Query<BatchQuery>, QueryRejection>,
) -> Result<Json<RecordingBatch>, ApiError> {
    let Query(query) = query.map_err(|_| {
        ApiError::BadRequest("Expected comma-separated recording IDs")
    })?;
    if query.ids.len() > 2100 {
        return Err(ApiError::BadRequest("At most 100 IDs are supported"));
    }
    let ids = if query.ids.is_empty() {
        vec![]
    } else {
        query
            .ids
            .split(',')
            .map(str::parse::<i64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                ApiError::BadRequest("Expected positive integer IDs")
            })?
    };
    let unique: std::collections::HashSet<_> = ids.iter().copied().collect();
    if ids.len() > 100
        || unique.len() != ids.len()
        || ids.iter().any(|id| *id <= 0)
    {
        return Err(ApiError::BadRequest(
            "Use at most 100 distinct positive recording IDs",
        ));
    }
    let items = db::browse::recording_batch(&pool, &ids).await?;
    let found: std::collections::HashSet<_> =
        items.iter().map(|r| r.id).collect();
    let missing_ids =
        ids.into_iter().filter(|id| !found.contains(id)).collect();
    Ok(Json(RecordingBatch { items, missing_ids }))
}

#[cfg(test)]
mod tests {
    //! Independent recording workflows against migrated SQLite databases.

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use crate::config::DocsConfig;

    use super::*;

    /// Keep the isolated database directory alive throughout each test.
    async fn setup() -> (TempDir, SqlitePool, Router) {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let app = crate::api::router(
            &crate::config::AuthConfig::default(),
            &DocsConfig::default(),
            &pool,
            crate::jobs::ScanService::new(pool.clone(), vec![]),
        );
        let app = crate::api::tests::with_admin(app, &pool).await;
        (directory, pool, app)
    }

    /// Send optional JSON through the router and parse its response.
    async fn request(
        app: &Router,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(path);
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
        let status = response.status();
        let headers = response.headers().clone();
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        (status, headers, serde_json::from_slice(&body).unwrap())
    }

    /// A recording needs neither a release nor a physical file to exist.
    #[tokio::test]
    async fn creates_and_reads_independent_recordings_with_ordered_artists() {
        let (_directory, pool, app) = setup().await;
        let mut ids = Vec::new();
        for name in ["First", "Second"] {
            let (_, _, artist) = request(
                &app,
                "POST",
                "/api/v1/artists",
                Some(json!({"name": name})),
            )
            .await;
            ids.push(artist["id"].as_i64().unwrap());
        }
        ids.reverse();
        let (status, headers, recording) = request(
            &app,
            "POST",
            "/api/v1/recordings",
            Some(json!({
                "title": " Запись ", "artist_ids": ids,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(recording["title"], "Запись");
        assert_eq!(recording["artist_ids"], json!(ids));
        let location = headers[header::LOCATION].to_str().unwrap();
        let (status, _, fetched) = request(&app, "GET", location, None).await;
        assert_eq!(status, StatusCode::OK);
        crate::api::tests::assert_base(&fetched, &recording);
        let (_, _, page) =
            request(&app, "GET", "/api/v1/recordings?limit=1", None).await;
        assert_eq!(page["items"], json!([fetched]));
        let (_, _, releases) =
            request(&app, "GET", "/api/v1/releases", None).await;
        assert_eq!(releases["items"], json!([]));
        pool.close().await;
    }

    /// Identical titles keep distinct IDs, and absent credits remain empty.
    #[tokio::test]
    async fn supports_duplicate_titles_and_bounded_pages() {
        let (_directory, pool, app) = setup().await;
        let (_, _, empty) =
            request(&app, "GET", "/api/v1/recordings", None).await;
        assert_eq!(
            empty,
            json!({"items": [], "limit": 50, "offset": 0,
            "total": 0,"has_more":false})
        );
        let mut recordings = Vec::new();
        for _ in 0..2 {
            let (status, _, recording) = request(
                &app,
                "POST",
                "/api/v1/recordings",
                Some(json!({"title": "Same"})),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
            assert_eq!(recording["artist_ids"], json!([]));
            recordings.push(recording);
        }
        assert_ne!(recordings[0]["id"], recordings[1]["id"]);
        let (_, _, page) =
            request(&app, "GET", "/api/v1/recordings?limit=1&offset=1", None)
                .await;
        crate::api::tests::assert_base(&page["items"][0], &recordings[1]);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        let (_, _, end) =
            request(&app, "GET", "/api/v1/recordings?offset=2", None).await;
        assert_eq!(end["items"], json!([]));
        pool.close().await;
    }

    /// Invalid fields and unknown credits cannot leave partial catalog rows.
    #[tokio::test]
    async fn rejects_invalid_fields_and_missing_artists() {
        let (_directory, pool, app) = setup().await;
        let (_, _, artist) = request(
            &app,
            "POST",
            "/api/v1/artists",
            Some(json!({"name": "Existing"})),
        )
        .await;
        let id = artist["id"].as_i64().unwrap();
        for payload in [
            json!({}),
            json!({"title": " "}),
            json!({"title": 42}),
            json!({"title": "a\0b"}),
            json!({"title": "я".repeat(256)}),
            json!({"title": "Valid", "release_id": 1}),
            json!({"title": "Valid", "file_path": "music.flac"}),
            json!({"title": "Valid", "artist_ids": [0]}),
            json!({"title": "Valid", "artist_ids": [id, id]}),
            json!({"title": "Valid", "artist_ids": [id, id + 1]}),
            json!({"title": "Valid", "artist_ids": (1..=101)
                .collect::<Vec<_>>()}),
        ] {
            let (status, _, error) =
                request(&app, "POST", "/api/v1/recordings", Some(payload))
                    .await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(error["error"]["code"], "validation_error");
        }
        let (_, _, page) =
            request(&app, "GET", "/api/v1/recordings", None).await;
        assert_eq!(page["items"], json!([]));
        pool.close().await;
    }

    /// Invalid queries and IDs use 400, while missing recordings use 404.
    #[tokio::test]
    async fn validates_queries_and_reports_missing_recordings() {
        let (_directory, pool, app) = setup().await;
        for path in [
            "/api/v1/recordings?limit=0",
            "/api/v1/recordings?limit=101",
            "/api/v1/recordings?offset=-1",
            "/api/v1/recordings?unknown=1",
            "/api/v1/recordings/abc",
            "/api/v1/recordings/9223372036854775808",
        ] {
            let (status, _, error) = request(&app, "GET", path, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["error"]["code"], "bad_request");
        }
        for path in ["/api/v1/recordings/1", "/api/v1/recordings/0"] {
            let (status, _, error) = request(&app, "GET", path, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(error["error"]["code"], "not_found");
        }
        pool.close().await;
    }

    /// Extraction and storage failures use safe shared JSON errors.
    #[tokio::test]
    async fn extraction_and_storage_errors_use_safe_envelopes() {
        let (_directory, pool, app) = setup().await;
        for (content, status) in
            [("text/plain", 415), ("application/json", 400)]
        {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/v1/recordings")
                        .header(header::CONTENT_TYPE, content)
                        .body(Body::from("{"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            let error: Value = serde_json::from_slice(&body).unwrap();
            assert!(error["error"]["code"].is_string());
        }
        pool.close().await;
        let (status, _, error) =
            request(&app, "GET", "/api/v1/recordings", None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            error,
            json!({"error": {
                "code": "internal_error", "message": "Internal server error",
            }})
        );
    }
}
