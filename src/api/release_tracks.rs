//! Nested track placement operations and their OpenAPI contracts.

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

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    db::release_tracks::{self, TrackError},
    library::release_tracks::{NewReleaseTrack, ReleaseTrack},
};

/// Register nested routes with the existing release parameter name `id`.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_track, list_tracks))
        .routes(routes!(get_track))
        .with_state(pool)
}

/// Recording placement; the release comes from the path, not the body.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CreateTrack {
    /// Existing recording to place in the tracklist.
    #[schema(minimum = 1)]
    recording_id: i64,
    /// One-based disc number; omitted means the first disc.
    #[serde(default = "first_disc")]
    #[schema(minimum = 1, default = 1)]
    disc_number: i64,
    /// One-based position within the disc; gaps are allowed.
    #[schema(minimum = 1)]
    position: i64,
}

/// Use the first disc for omitted disc numbers.
fn first_disc() -> i64 {
    1
}

/// Bounded tracklist slice ordered by disc number and position.
#[derive(Serialize, ToSchema)]
struct TrackPage {
    /// Track occurrences, independent of physical audio files.
    items: Vec<ReleaseTrack>,
    /// Maximum returned occurrences.
    limit: i64,
    /// Number of skipped occurrences in tracklist order.
    offset: i64,
}

impl From<TrackError> for ApiError {
    /// Map expected catalog failures to safe HTTP responses.
    fn from(error: TrackError) -> Self {
        match error {
            TrackError::MissingRelease => Self::NotFound,
            TrackError::MissingRecording => {
                Self::Validation("Recording does not exist")
            }
            TrackError::PositionOccupied => {
                Self::Conflict("Track position is already occupied")
            }
            TrackError::Storage(error) => error.into(),
        }
    }
}

/// Place a recording in a release; recordings may be reused at other slots.
///
/// The parent must exist. Occupied disc/position pairs return 409 without
/// changing the tracklist; nonexistent recordings return 422.
#[utoipa::path(
    post, path = "/releases/{id}/tracks", tag = "release_tracks",
    operation_id = "create_release_track", request_body = CreateTrack,
    params(("id" = i64, Path, minimum = 1, description = "Release ID")),
    responses(
        (status = 201, body = ReleaseTrack, description = "Track created",
            headers(("Location" = String, description = "Track URL"))),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn create_track(
    State(pool): State<SqlitePool>,
    OriginalUri(uri): OriginalUri,
    release_id: Result<Path<i64>, PathRejection>,
    payload: Result<Json<CreateTrack>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(release_id) =
        release_id.map_err(|_| ApiError::BadRequest("Invalid release id"))?;
    let Json(payload) = payload.map_err(ApiError::from)?;
    let input = NewReleaseTrack::new(
        payload.recording_id,
        payload.disc_number,
        payload.position,
    )
    .map_err(ApiError::Validation)?;
    let track = release_tracks::create(&pool, release_id, input).await?;
    let location = format!("{}/{}", uri.path(), track.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(track),
    )
        .into_response())
}

/// Retrieve a track occurrence belonging to the specified release.
#[utoipa::path(
    get, path = "/releases/{id}/tracks/{track_id}", tag = "release_tracks",
    operation_id = "get_release_track",
    params(
        ("id" = i64, Path, minimum = 1, description = "Release ID"),
        ("track_id" = i64, Path, minimum = 1, description = "Track ID")
    ),
    responses(
        (status = 200, body = ReleaseTrack, description = "Track found"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn get_track(
    State(pool): State<SqlitePool>,
    ids: Result<Path<(i64, i64)>, PathRejection>,
) -> Result<Json<ReleaseTrack>, ApiError> {
    let Path((release_id, track_id)) =
        ids.map_err(|_| ApiError::BadRequest("Invalid release or track id"))?;
    Ok(Json(
        release_tracks::find(&pool, release_id, track_id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// Read a release's tracklist in disc and position order.
#[utoipa::path(
    get, path = "/releases/{id}/tracks", tag = "release_tracks",
    operation_id = "list_release_tracks",
    params(("id" = i64, Path, minimum = 1, description = "Release ID"),
        Pagination),
    responses(
        (status = 200, body = TrackPage, description = "Ordered track page"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn list_tracks(
    State(pool): State<SqlitePool>,
    release_id: Result<Path<i64>, PathRejection>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<TrackPage>, ApiError> {
    let Path(release_id) =
        release_id.map_err(|_| ApiError::BadRequest("Invalid release id"))?;
    let Query(query) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    query.validate()?;
    let items =
        release_tracks::list(&pool, release_id, query.limit, query.offset)
            .await?;
    Ok(Json(TrackPage {
        items,
        limit: query.limit,
        offset: query.offset,
    }))
}

#[cfg(test)]
mod tests {
    //! Nested HTTP contracts, ordering, reference failures, and conflicts.

    use crate::{
        config::DocsConfig,
        db,
        library::{
            recordings::NewRecording,
            releases::{NewRelease, ReleaseKind},
        },
    };
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;

    /// Seed a migrated catalog and retain its directory throughout the test.
    async fn setup() -> (TempDir, SqlitePool, Router, i64, i64) {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let release = db::releases::create(
            &pool,
            NewRelease::new("Album", ReleaseKind::Album, vec![]).unwrap(),
        )
        .await
        .unwrap();
        let recording = db::recordings::create(
            &pool,
            NewRecording::new("Recording", vec![]).unwrap(),
        )
        .await
        .unwrap();
        let app = crate::api::router(&DocsConfig::default(), &pool);
        (directory, pool, app, release.id, recording.id)
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

    /// Creation defaults to disc one and returns a readable nested Location.
    #[tokio::test]
    async fn creates_and_fetches_track_occurrences() {
        let (_directory, pool, app, release, recording) = setup().await;
        let path = format!("/api/v1/releases/{release}/tracks");
        let (status, headers, track) = request(
            &app,
            "POST",
            &path,
            Some(json!({"recording_id": recording, "position": 1})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(track["disc_number"], 1);
        assert_eq!(track["release_id"], release);
        assert_eq!(track["recording_id"], recording);
        let location = headers[header::LOCATION].to_str().unwrap();
        assert_eq!(location, format!("{path}/{}", track["id"]));
        let (status, _, fetched) = request(&app, "GET", location, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched, track);

        let (_, _, other) = request(
            &app,
            "POST",
            "/api/v1/releases",
            Some(json!({"title": "Other", "kind": "album"})),
        )
        .await;
        let other_path = format!("/api/v1/releases/{}/tracks", other["id"]);
        let (status, _, _) = request(
            &app,
            "GET",
            &format!("{other_path}/{}", track["id"]),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _, reused) = request(
            &app,
            "POST",
            &other_path,
            Some(json!({"recording_id": recording, "position": 1})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_ne!(reused["id"], track["id"]);
        pool.close().await;
    }

    /// Tracklist pages use musical order rather than insertion order.
    #[tokio::test]
    async fn lists_tracks_by_disc_and_position() {
        let (_directory, pool, app, release, recording) = setup().await;
        let path = format!("/api/v1/releases/{release}/tracks");
        let (_, _, empty) = request(&app, "GET", &path, None).await;
        assert_eq!(empty, json!({"items": [], "limit": 50, "offset": 0}));
        for (disc, position) in [(2, 1), (1, 3), (1, 1)] {
            let (status, _, _) = request(
                &app,
                "POST",
                &path,
                Some(json!({
                    "recording_id": recording, "disc_number": disc,
                    "position": position,
                })),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }
        let (_, _, page) = request(&app, "GET", &path, None).await;
        let positions: Vec<_> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|track| {
                (
                    track["disc_number"].as_i64().unwrap(),
                    track["position"].as_i64().unwrap(),
                )
            })
            .collect();
        assert_eq!(positions, [(1, 1), (1, 3), (2, 1)]);
        let (_, _, page) =
            request(&app, "GET", &format!("{path}?limit=1&offset=1"), None)
                .await;
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["position"], 3);
        let (_, _, end) =
            request(&app, "GET", &format!("{path}?offset=3"), None).await;
        assert_eq!(end["items"], json!([]));
        pool.close().await;
    }

    /// Occupied slots and missing references preserve the existing tracklist.
    #[tokio::test]
    async fn conflicts_and_unknown_references_use_stable_statuses() {
        let (_directory, pool, app, release, recording) = setup().await;
        let path = format!("/api/v1/releases/{release}/tracks");
        let payload = json!({"recording_id": recording, "position": 1});
        let (_, _, track) =
            request(&app, "POST", &path, Some(payload.clone())).await;
        let (status, _, error) =
            request(&app, "POST", &path, Some(payload.clone())).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "conflict");
        let (status, _, error) = request(
            &app,
            "POST",
            &path,
            Some(json!({
                "recording_id": recording + 1, "position": 2,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error["error"]["code"], "validation_error");
        let missing = format!("/api/v1/releases/{}/tracks", release + 1);
        for (method, body) in [("GET", None), ("POST", Some(payload))] {
            let (status, _, error) =
                request(&app, method, &missing, body).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(error["error"]["code"], "not_found");
        }
        let (_, _, page) = request(&app, "GET", &path, None).await;
        assert_eq!(page["items"], json!([track]));
        pool.close().await;
    }

    /// Invalid bodies and pagination never create track occurrences.
    #[tokio::test]
    async fn validates_body_parameters_and_nested_identifiers() {
        let (_directory, pool, app, release, recording) = setup().await;
        let path = format!("/api/v1/releases/{release}/tracks");
        for body in [
            json!({}),
            json!({"recording_id": 0, "position": 1}),
            json!({"recording_id": recording, "position": 0}),
            json!({"recording_id": recording, "position": -1}),
            json!({"recording_id": recording,
                "position": 1, "disc_number": 0}),
            json!({"recording_id": recording, "position": "1"}),
            json!({"recording_id": recording, "position": 1, "title": "X"}),
        ] {
            let (status, _, error) =
                request(&app, "POST", &path, Some(body)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(error["error"]["code"], "validation_error");
        }
        for query in ["limit=0", "limit=101", "offset=-1", "unknown=1"] {
            let (status, _, error) =
                request(&app, "GET", &format!("{path}?{query}"), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["error"]["code"], "bad_request");
        }
        for invalid in
            ["/api/v1/releases/abc/tracks".into(), format!("{path}/abc")]
        {
            let (status, _, _) = request(&app, "GET", &invalid, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        let (_, _, page) = request(&app, "GET", &path, None).await;
        assert_eq!(page["items"], json!([]));
        pool.close().await;
    }

    /// Media, syntax, and storage errors keep the common safe envelope.
    #[tokio::test]
    async fn extraction_and_storage_failures_return_safe_json() {
        let (_directory, pool, app, release, _) = setup().await;
        let path = format!("/api/v1/releases/{release}/tracks");
        for (content, status) in
            [("text/plain", 415), ("application/json", 400)]
        {
            let response = app
                .clone()
                .oneshot(
                    Request::post(&path)
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
        let (status, _, error) = request(&app, "GET", &path, None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            error,
            json!({"error": {
                "code": "internal_error", "message": "Internal server error",
            }})
        );
    }
}
