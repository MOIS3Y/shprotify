//! Administrative scan admission and persisted, bounded progress reports.

use axum::{
    Json,
    extract::{
        OriginalUri, Path, Query, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{
    db,
    jobs::{ScanIssue, ScanJob, ScanService, StartError},
};

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};

/// State shared only by the scan routes, leaving catalog handlers unchanged.
#[derive(Clone)]
struct ScanState {
    /// Durable history and error reports.
    pool: SqlitePool,
    /// Bounded executor owned by application startup and shutdown.
    service: ScanService,
}

impl axum::extract::FromRef<ScanState> for SqlitePool {
    /// Expose the pool to generic authentication guards for scan routes.
    fn from_ref(state: &ScanState) -> Self {
        state.pool.clone()
    }
}

/// Register version-local scan operations and their shared OpenAPI schemas.
pub(super) fn router(pool: SqlitePool, service: ScanService) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(start_scan, list_scans))
        .routes(routes!(get_scan))
        .routes(routes!(list_scan_errors))
        .with_state(ScanState { pool, service })
}

/// Optional scan policy; the default reuses successfully committed probes.
#[derive(Default, Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
#[into_params(parameter_in = Query)]
struct ScanOptions {
    /// Bypass filesystem change detection and reread every regular file.
    #[param(default = false)]
    force: bool,
}

/// Bounded history in ascending job ID order.
#[derive(Serialize, ToSchema)]
struct ScanPage {
    /// Persisted jobs, including completed and interrupted work.
    items: Vec<ScanJob>,
    /// Effective maximum page size.
    limit: i64,
    /// Number of skipped jobs.
    offset: i64,
}

/// Bounded per-entry error report in ascending issue ID order.
#[derive(Serialize, ToSchema)]
struct ScanErrorPage {
    /// Per-file failures and nonfatal association issues.
    items: Vec<ScanIssue>,
    /// Effective maximum page size.
    limit: i64,
    /// Number of skipped issues.
    offset: i64,
}

/// Start indexing all configured roots outside the HTTP request.
///
/// No body is required. Roots come exclusively from server configuration.
/// New paths may share explicit recording IDs. Known paths preserve catalog
/// metadata while refreshing properties/source tags. One scan runs at a time;
/// admission is persisted.
/// New-file artist/release tags match within the immediate parent directory.
/// Occupied track positions are reported without discarding imported files.
/// Unchanged audio files are skipped unless `force=true` is requested.
///
/// Requires the administrator role.
#[utoipa::path(
    post, path = "/library/scans", tag = "library_scans",
    operation_id = "start_library_scan", params(ScanOptions),
    security(("personal_key" = [])),
    responses(
        (status = 202, body = ScanJob, description = "Scan accepted",
            headers(("Location" = String, description = "Scan status URL"))),
        (status = 400, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn start_scan(
    _admin: crate::auth::Admin,
    State(state): State<ScanState>,
    OriginalUri(uri): OriginalUri,
    query: Result<Query<ScanOptions>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(options) =
        query.map_err(|_| ApiError::BadRequest("Invalid scan options"))?;
    let job =
        state.service.start(options.force).await.map_err(
            |error| match error {
                StartError::Busy => {
                    ApiError::Conflict("A library scan is running")
                }
                StartError::Stopping => {
                    ApiError::Conflict("Scanner is shutting down")
                }
                StartError::Storage(error) => ApiError::from(error),
            },
        )?;
    let location = format!("{}/{}", uri.path(), job.id);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(job),
    )
        .into_response())
}

/// Fetch persisted status; completed jobs may contain isolated entry errors.
///
/// Requires the administrator role.
#[utoipa::path(
    get, path = "/library/scans/{id}", tag = "library_scans",
    operation_id = "get_library_scan",
    params(("id" = i64, Path, description = "Scan identifier", minimum = 1)),
    security(("personal_key" = [])),
    responses(
        (status = 200, body = ScanJob, description = "Scan status"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn get_scan(
    _admin: crate::auth::Admin,
    State(state): State<ScanState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Json<ScanJob>, ApiError> {
    let Path(id) =
        path.map_err(|_| ApiError::BadRequest("Invalid scan ID"))?;
    Ok(Json(
        db::scans::find(&state.pool, id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// List history with bounded pagination, including jobs from older processes.
///
/// Requires the administrator role.
#[utoipa::path(
    get, path = "/library/scans", tag = "library_scans",
    operation_id = "list_library_scans", params(Pagination),
    security(("personal_key" = [])),
    responses(
        (status = 200, body = ScanPage, description = "Scan history"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn list_scans(
    _admin: crate::auth::Admin,
    State(state): State<ScanState>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<ScanPage>, ApiError> {
    let Query(page) =
        query.map_err(|_| ApiError::BadRequest("Invalid query"))?;
    page.validate()?;
    let items = db::scans::list(&state.pool, page.limit, page.offset).await?;
    Ok(Json(ScanPage {
        items,
        limit: page.limit,
        offset: page.offset,
    }))
}

/// Read file errors and matching issues without exposing SQL diagnostics.
///
/// Requires the administrator role.
#[utoipa::path(
    get, path = "/library/scans/{id}/errors", tag = "library_scans",
    operation_id = "list_library_scan_errors", params(Pagination,
        ("id" = i64, Path, description = "Scan identifier", minimum = 1)),
    security(("personal_key" = [])),
    responses(
        (status = 200, body = ScanErrorPage, description = "Scan errors"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn list_scan_errors(
    _admin: crate::auth::Admin,
    State(state): State<ScanState>,
    path: Result<Path<i64>, PathRejection>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<ScanErrorPage>, ApiError> {
    let Path(id) =
        path.map_err(|_| ApiError::BadRequest("Invalid scan ID"))?;
    let Query(page) =
        query.map_err(|_| ApiError::BadRequest("Invalid query"))?;
    page.validate()?;
    db::scans::find(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let items =
        db::scans::errors(&state.pool, id, page.limit, page.offset).await?;
    Ok(Json(ScanErrorPage {
        items,
        limit: page.limit,
        offset: page.offset,
    }))
}

#[cfg(test)]
mod tests {
    //! Background admission, persistent reports, and safe HTTP failures.

    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::config::DocsConfig;

    /// Send requests through the entire versioned API and decode JSON.
    async fn request(
        app: &Router,
        method: &str,
        path: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        (status, headers, serde_json::from_slice(&body).unwrap())
    }

    /// Accepted work produces catalog data, source tags, and a durable report.
    #[tokio::test]
    async fn indexes_configured_roots_and_exposes_progress_and_source_tags() {
        let directory = tempfile::tempdir().unwrap();
        let music = directory.path().join("music");
        std::fs::create_dir(&music).unwrap();
        std::fs::write(
            music.join("song.wav"),
            crate::library::metadata::tests::wave(),
        )
        .unwrap();
        std::fs::write(music.join("bad.mp3"), b"damaged").unwrap();
        let pool = db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let service = ScanService::new(pool.clone(), vec![music]);
        let app = crate::api::router(
            &crate::config::AuthConfig::default(),
            &DocsConfig::default(),
            &pool,
            service.clone(),
        );
        let app = crate::api::tests::with_admin(app, &pool).await;
        let path = "/api/v1/library/scans";
        let (status, _, empty) = request(&app, "GET", path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(empty, json!({"items": [], "limit": 50, "offset": 0}));
        let (status, headers, job) = request(&app, "POST", path).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(job["status"], "running");
        assert_eq!(job["roots"].as_array().unwrap().len(), 1);
        let location = headers[header::LOCATION].to_str().unwrap();
        assert_eq!(location, format!("{path}/{}", job["id"]));
        service.close().await;
        let (status, _, result) = request(&app, "GET", location).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["status"], "completed");
        assert_eq!(result["imported"], 1);
        assert_eq!(result["errors"], 1);
        assert!(result["finished_at"].is_string());
        let (_, _, errors) =
            request(&app, "GET", &format!("{location}/errors?limit=1")).await;
        assert_eq!(errors["items"].as_array().unwrap().len(), 1);
        let (_, _, empty) =
            request(&app, "GET", &format!("{location}/errors?offset=1")).await;
        assert_eq!(empty["items"], json!([]));
        let (_, _, history) =
            request(&app, "GET", &format!("{path}?limit=1")).await;
        assert_eq!(history["items"][0], result);
        let (file, recording): (i64, i64) =
            sqlx::query_as("SELECT id, recording_id FROM audio_files")
                .fetch_one(&pool)
                .await
                .unwrap();
        let tags_path =
            format!("/api/v1/recordings/{recording}/files/{file}/tags");
        let (status, _, tags) = request(&app, "GET", &tags_path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(tags["title"], "Source title");
        assert_eq!(tags["artist"], "Source artist");
        assert_eq!(tags["track_number"], 7);
        let (_, _, releases) = request(&app, "GET", "/api/v1/releases").await;
        assert_eq!(releases["items"].as_array().unwrap().len(), 1);
        let release = &releases["items"][0];
        assert_eq!(release["title"], "Source album");
        assert_eq!(release["artist_ids"], json!([]));
        let (_, _, tracks) = request(
            &app,
            "GET",
            &format!("/api/v1/releases/{}/tracks", release["id"],),
        )
        .await;
        assert_eq!(tracks["items"][0]["recording_id"], recording);
        assert_eq!(tracks["items"][0]["position"], 7);
        assert_eq!(tracks["items"][0]["disc_number"], 1);
        let wrong =
            format!("/api/v1/recordings/{}/files/{file}/tags", recording + 1);
        assert_eq!(
            request(&app, "GET", &wrong).await.0,
            StatusCode::NOT_FOUND
        );
        pool.close().await;
    }

    /// Conflicts, invalid paging/IDs, and missing resources use shared errors.
    #[tokio::test]
    async fn rejects_conflicts_and_invalid_report_queries() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let service = ScanService::new(pool.clone(), vec![]);
        let app = crate::api::router(
            &crate::config::AuthConfig::default(),
            &DocsConfig::default(),
            &pool,
            service.clone(),
        );
        let app = crate::api::tests::with_admin(app, &pool).await;
        let active = db::scans::create(&pool, &[], false).await.unwrap();
        let path = "/api/v1/library/scans";
        for query in ["force=yes", "force=1", "unknown=true", "force="] {
            let (status, _, error) =
                request(&app, "POST", &format!("{path}?{query}")).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["error"]["code"], "bad_request");
        }
        let (status, _, error) = request(&app, "POST", path).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["error"]["code"], "conflict");
        let location = format!("{path}/{}", active.id);
        for query in ["limit=0", "limit=101", "offset=-1", "unknown=1"] {
            for target in [
                format!("{path}?{query}"),
                format!("{location}/errors?{query}"),
            ] {
                assert_eq!(
                    request(&app, "GET", &target).await.0,
                    StatusCode::BAD_REQUEST
                );
            }
        }
        for target in [format!("{path}/bad"), format!("{path}/bad/errors")] {
            assert_eq!(
                request(&app, "GET", &target).await.0,
                StatusCode::BAD_REQUEST
            );
        }
        for target in [
            format!("{path}/999"),
            format!("{path}/999/errors"),
            "/api/v1/recordings/1/files/1/tags".into(),
        ] {
            assert_eq!(
                request(&app, "GET", &target).await.0,
                StatusCode::NOT_FOUND
            );
        }
        service.close().await;
        pool.close().await;
        let (status, _, error) = request(&app, "GET", path).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error["error"]["message"], "Internal server error");
    }

    /// Operations preserve the version prefix and document status schemas.
    #[tokio::test]
    async fn documents_scan_and_observed_tag_contracts() {
        let pool = SqlitePool::connect_lazy("sqlite::memory:").unwrap();
        let service = ScanService::new(pool.clone(), vec![]);
        let app = crate::api::router(
            &crate::config::AuthConfig::default(),
            &DocsConfig { enabled: true },
            &pool,
            service,
        );
        let (_, _, schema) = request(&app, "GET", "/openapi.json").await;
        let post = &schema["paths"]["/api/v1/library/scans"]["post"];
        assert!(post["responses"]["202"]["headers"]["Location"].is_object());
        assert!(post["responses"]["409"].is_object());
        let force = post["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == "force")
            .unwrap();
        assert_eq!(force["schema"]["type"], "boolean");
        assert_eq!(force["schema"]["default"], false);
        let properties =
            &schema["components"]["schemas"]["ScanJob"]["properties"];
        assert_eq!(properties["unchanged"]["type"], "integer");
        assert!(schema["paths"]["/api/v1/library/scans/{id}/errors"]
            ["get"]["responses"]["404"].is_object());
        assert!(
            schema["paths"]["/api/v1/recordings/{id}/files/{file_id}/tags"]
                .is_object()
        );
        assert_eq!(
            schema["components"]["schemas"]["ScanStatus"]["enum"],
            json!(["running", "completed", "failed", "interrupted"])
        );
        assert!(schema["components"]["schemas"]["AudioTags"].is_object());
    }

    /// Query-selected force bypasses persisted snapshots through the full API.
    #[tokio::test]
    async fn force_query_rereads_unchanged_audio_and_persists_policy() {
        let directory = tempfile::tempdir().unwrap();
        let music = directory.path().join("music");
        std::fs::create_dir(&music).unwrap();
        std::fs::write(
            music.join("song.wav"),
            crate::library::metadata::tests::wave(),
        )
        .unwrap();
        let pool = db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        for (query, force, refreshed, unchanged) in [
            ("", false, 0, 0),
            ("?force=true", true, 1, 0),
            ("?force=false", false, 0, 1),
        ] {
            let service = ScanService::new(pool.clone(), vec![music.clone()]);
            let app = crate::api::router(
                &crate::config::AuthConfig::default(),
                &DocsConfig::default(),
                &pool,
                service.clone(),
            );
            let app = crate::api::tests::with_admin(app, &pool).await;
            let (status, headers, accepted) = request(
                &app,
                "POST",
                &format!("/api/v1/library/scans{query}"),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(accepted["force"], force);
            service.close().await;
            let location = headers[header::LOCATION].to_str().unwrap();
            assert!(!location.contains('?'));
            let (_, _, result) = request(&app, "GET", location).await;
            assert_eq!(result["status"], "completed");
            assert_eq!(result["refreshed"], refreshed);
            assert_eq!(result["unchanged"], unchanged);
        }
        pool.close().await;
    }
}
