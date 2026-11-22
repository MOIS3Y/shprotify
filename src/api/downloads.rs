//! Authenticated member acquisition; provider failures stay in owned jobs.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    auth::CurrentUser,
    db,
    downloads::{DownloadError, DownloadJob, SearchRequest, Source},
    jobs::downloads::DownloadService,
};
use axum::{
    Json,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Authentication storage and the application-owned bounded worker lifecycle.
#[derive(Clone)]
struct DownloadState {
    /// Same pool used for credentials, ownership and queue admission.
    pool: SqlitePool,
    /// Provider admission, cancellation supervision and managed imports.
    service: DownloadService,
}
impl axum::extract::FromRef<DownloadState> for SqlitePool {
    /// Authenticate against this instance's persisted accounts.
    fn from_ref(state: &DownloadState) -> Self {
        state.pool.clone()
    }
}

/// Register stable routes even when acquisition is disabled at startup.
pub(super) fn router(
    pool: SqlitePool,
    service: DownloadService,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(providers))
        .routes(routes!(search))
        .routes(routes!(list_jobs, download))
        .routes(routes!(get_job))
        .routes(routes!(sources))
        .routes(routes!(cancel))
        .routes(routes!(retry))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(DownloadState { pool, service })
}

/// Selection must originate from this account's recent successful search.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct DownloadRequest {
    /// Owned source ID; no URLs, paths or yt-dlp flags are accepted.
    source_id: i64,
}

/// Provider capabilities are configuration, not a guarantee of availability.
#[derive(Serialize, ToSchema)]
struct Provider {
    /// Stable adapter name.
    id: &'static str,
    /// Human display name.
    name: &'static str,
    /// Explicit instability notice for clients.
    experimental: bool,
    /// Whether this instance accepts jobs for this provider.
    enabled: bool,
    /// Supported operations; source lookup and download are both asynchronous.
    capabilities: [&'static str; 2],
}

/// Owned job history page, newest first.
#[derive(Serialize, ToSchema)]
struct JobPage {
    /// Terminal and active jobs belonging to the current account.
    items: Vec<DownloadJob>,
    /// Requested page size.
    limit: i64,
    /// Number of jobs skipped.
    offset: i64,
}

/// Keep domain errors safe and never expose subprocess output or filesystem
/// paths.
impl From<DownloadError> for ApiError {
    fn from(error: DownloadError) -> Self {
        match error {
            DownloadError::NotFound => Self::NotFound,
            DownloadError::Invalid(message) => Self::Validation(message),
            DownloadError::Conflict(message) => Self::Conflict(message),
            error => Self::Internal(error.into()),
        }
    }
}

/// Every personal provider/job response is private and must not be cached.
fn private(value: impl IntoResponse) -> Response {
    let mut response = value.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Advertise the polling resource after its durable admission and supervision.
fn accepted(job: DownloadJob) -> Response {
    let location = format!("/api/v1/me/downloads/{}", job.id);
    private((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(job),
    ))
}

/// Parse positive IDs after credential authentication and before database
/// access.
fn identifier(
    path: Result<Path<i64>, PathRejection>,
) -> Result<i64, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::BadRequest("Invalid job ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("ID must be positive"));
    }
    Ok(id)
}

/// List configured acquisition capabilities; experimental availability may
/// change.
#[utoipa::path(get, path = "/downloads/providers", tag = "Downloads",
    operation_id = "downloads_providers", security(("personal_key" = [])),
    responses((status = 200, description = "Owned acquisition result",
            body = Vec<Provider>),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)))]
async fn providers(
    CurrentUser(_user): CurrentUser,
    State(state): State<DownloadState>,
) -> Result<Response, ApiError> {
    Ok(private(Json(vec![Provider {
        id: "youtube",
        name: "YouTube",
        experimental: true,
        enabled: state.service.enabled(),
        capabilities: ["search", "download"],
    }])))
}

/// Queue a bounded YouTube search for a recording, Discovery candidate or
/// explicit query.
#[utoipa::path(post, path = "/me/downloads/searches", tag = "Downloads",
    operation_id = "downloads_search", security(("personal_key" = [])),
    request_body = SearchRequest,
    responses((status = 202, description = "Owned acquisition result",
            body = DownloadJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)))]
async fn search(
    CurrentUser(user): CurrentUser,
    State(state): State<DownloadState>,
    body: Result<Json<SearchRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body?;
    Ok(accepted(state.service.search(user.id, request).await?))
}

/// List only this account’s persisted acquisition history, newest first.
#[utoipa::path(get, path = "/me/downloads", tag = "Downloads",
    operation_id = "downloads_list_jobs", security(("personal_key" = [])),
    params(Pagination),
    responses((status = 200, description = "Owned acquisition result",
            body = JobPage),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)))]
async fn list_jobs(
    CurrentUser(user): CurrentUser,
    State(state): State<DownloadState>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(page) =
        query.map_err(|_| ApiError::BadRequest("Invalid pagination"))?;
    page.validate()?;
    let items =
        db::downloads::jobs(&state.pool, user.id, page.limit, page.offset)
            .await?;
    Ok(private(Json(JobPage {
        items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// Queue one selected source from an owned search completed within the last
/// hour.
#[utoipa::path(post, path = "/me/downloads", tag = "Downloads",
    operation_id = "downloads_download", security(("personal_key" = [])),
    request_body = DownloadRequest,
    responses((status = 202, description = "Owned acquisition result",
            body = DownloadJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)))]
async fn download(
    CurrentUser(user): CurrentUser,
    State(state): State<DownloadState>,
    body: Result<Json<DownloadRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body?;
    if request.source_id <= 0 {
        return Err(ApiError::Validation("Invalid source ID"));
    }
    Ok(accepted(
        state.service.download(user.id, request.source_id).await?,
    ))
}

/// Read an owned job, progress, safe failure or final catalog IDs.
#[utoipa::path(get, path = "/me/downloads/{id}", tag = "Downloads",
    operation_id = "downloads_get_job", security(("personal_key" = [])),
    params(("id" = i64, Path, minimum = 1)),
    responses((status = 200, description = "Owned acquisition result",
            body = DownloadJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)))]
async fn get_job(
    CurrentUser(user): CurrentUser,
    State(state): State<DownloadState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    Ok(private(Json(
        db::downloads::job(&state.pool, user.id, id).await?,
    )))
}

/// Read at most five stable source results of an owned search job.
#[utoipa::path(get, path = "/me/downloads/{id}/sources", tag = "Downloads",
    operation_id = "downloads_sources", security(("personal_key" = [])),
    params(("id" = i64, Path, minimum = 1)),
    responses((status = 200, description = "Owned acquisition result",
            body = Vec<Source>),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)))]
async fn sources(
    CurrentUser(user): CurrentUser,
    State(state): State<DownloadState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    Ok(private(Json(
        db::downloads::sources(&state.pool, user.id, id).await?,
    )))
}

/// Cancel queued or provider work; importing is too late to cancel. Terminal
/// requests are idempotent.
#[utoipa::path(post, path = "/me/downloads/{id}/cancel", tag = "Downloads",
    operation_id = "downloads_cancel", security(("personal_key" = [])),
    params(("id" = i64, Path, minimum = 1)),
    responses((status = 200, description = "Owned acquisition result",
            body = DownloadJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)))]
async fn cancel(
    CurrentUser(user): CurrentUser,
    State(state): State<DownloadState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    Ok(private(Json(
        db::downloads::cancel(&state.pool, user.id, id).await?,
    )))
}

/// Explicitly retry unsuccessful work after rechecking access and resource
/// limits.
#[utoipa::path(post, path = "/me/downloads/{id}/retry", tag = "Downloads",
    operation_id = "downloads_retry", security(("personal_key" = [])),
    params(("id" = i64, Path, minimum = 1)),
    responses((status = 202, description = "Owned acquisition result",
            body = DownloadJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)))]
async fn retry(
    CurrentUser(user): CurrentUser,
    State(state): State<DownloadState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    Ok(accepted(state.service.retry(user.id, id).await?))
}

#[cfg(test)]
mod tests {
    //! Real member credentials exercise acquisition and authenticated
    //! streaming.
    use super::*;
    use crate::jobs::downloads::tests::Fixture;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Mount production routes with a fake process, not an authentication
    /// bypass.
    async fn app(f: &Fixture) -> (Router, String, String) {
        let alice = crate::auth::token().unwrap();
        let bob = crate::auth::token().unwrap();
        db::users::issue_by_id(&f.pool, 1, &crate::auth::digest(&alice))
            .await
            .unwrap();
        db::users::issue_by_id(&f.pool, 2, &crate::auth::digest(&bob))
            .await
            .unwrap();
        let scans = crate::jobs::ScanService::new(f.pool.clone(), vec![]);
        let metadata = crate::jobs::metadata::MetadataService::new(
            f.pool.clone(),
            scans.roots(),
            crate::config::MetadataConfig::default(),
        )
        .unwrap();
        let discovery = crate::jobs::discovery::DiscoveryService::new(
            f.pool.clone(),
            crate::config::DiscoveryConfig::default(),
        )
        .unwrap();
        let app = crate::api::router_with_services(
            &crate::config::AuthConfig::default(),
            &crate::config::DocsConfig { enabled: true },
            &f.pool,
            crate::api::Services {
                scans,
                metadata,
                discovery,
                downloads: f.service.clone(),
                automation: crate::config::AutomationConfig::default(),
                clients: crate::config::ClientSettings::default(),
                removals: crate::jobs::removals::RemovalService::new(
                    f.pool.clone(),
                    None,
                ),
            },
        );
        (app, alice, bob)
    }

    /// Send JSON through Axum and retain status/headers for contract
    /// assertions.
    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        let mut request = Request::builder().method(method).uri(uri);
        if !token.is_empty() {
            request = request
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = if let Some(body) = body {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        } else {
            Body::empty()
        };
        let response = app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 512 * 1024).await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap();
        (status, headers, value)
    }

    /// Members search, download and stream; other accounts see only shared
    /// audio.
    #[tokio::test]
    async fn members_acquire_and_stream_shared_audio() {
        let f = Fixture::new().await;
        let (app, alice, bob) = app(&f).await;
        let (status, _, providers) =
            call(&app, "GET", "/api/v1/downloads/providers", &alice, None)
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(providers[0]["enabled"], true);
        assert_eq!(providers[0]["experimental"], true);
        let (status, headers, search) = call(
            &app,
            "POST",
            "/api/v1/me/downloads/searches",
            &alice,
            Some(json!({"kind":"query", "title":"Chosen title",
                "artists":["Chosen artist"]})),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{search}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        let id = search["id"].as_i64().unwrap();
        assert_eq!(
            headers[header::LOCATION],
            format!("/api/v1/me/downloads/{id}")
        );
        assert_eq!(f.wait(1, id).await.status, "completed");
        let path = format!("/api/v1/me/downloads/{id}/sources");
        let (_, _, sources) = call(&app, "GET", &path, &alice, None).await;
        assert_eq!(
            call(&app, "GET", &path, &bob, None).await.0,
            StatusCode::NOT_FOUND
        );
        let selected = json!({"source_id":sources[0]["id"]});
        assert_eq!(
            call(
                &app,
                "POST",
                "/api/v1/me/downloads",
                &bob,
                Some(selected.clone())
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let (status, _, job) =
            call(&app, "POST", "/api/v1/me/downloads", &alice, Some(selected))
                .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{job}");
        let done = f.wait(1, job["id"].as_i64().unwrap()).await;
        assert_eq!(done.status, "completed");
        let path = format!(
            "/api/v1/recordings/{}/files/{}/stream",
            done.recording_id.unwrap(),
            done.file_id.unwrap()
        );
        let response = app
            .clone()
            .oneshot(
                Request::get(&path)
                    .header(header::AUTHORIZATION, format!("Bearer {bob}"))
                    .header(header::RANGE, "bytes=0-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            to_bytes(response.into_body(), 10).await.unwrap().as_ref(),
            b"RIFF"
        );
        let (status, _, _) =
            call(&app, "GET", "/api/v1/me/downloads", &bob, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("/api/v1/me/downloads/{id}/cancel"),
                &bob,
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let (status, _, error) = call(
            &app,
            "POST",
            "/api/v1/recordings",
            &alice,
            Some(json!({"title":"Forbidden", "artist_ids":[]})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
        f.service.close().await;
        f.pool.close().await;
    }

    /// Credentials precede validation; source contracts reject arbitrary
    /// options.
    #[tokio::test]
    async fn validates_auth_requests_and_schema() {
        let f = Fixture::new().await;
        let (app, alice, _) = app(&f).await;
        for path in [
            "/api/v1/downloads/providers",
            "/api/v1/me/downloads?limit=0",
            "/api/v1/me/downloads/nope",
        ] {
            assert_eq!(
                call(&app, "GET", path, "", None).await.0,
                StatusCode::UNAUTHORIZED
            );
        }
        for body in [
            json!({"kind":"query","title":"X","artists":[],
                "url":"http://localhost"}),
            json!({"kind":"recording","id":1,"exec":"shell"}),
            json!({"kind":"query","title":"", "artists":[]}),
        ] {
            assert_eq!(
                call(
                    &app,
                    "POST",
                    "/api/v1/me/downloads/searches",
                    &alice,
                    Some(body)
                )
                .await
                .0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        for query in ["limit=101", "offset=-1", "unknown=yes"] {
            assert_eq!(
                call(
                    &app,
                    "GET",
                    &format!("/api/v1/me/downloads?{query}"),
                    &alice,
                    None
                )
                .await
                .0,
                StatusCode::BAD_REQUEST
            );
        }
        let (_, _, schema) =
            call(&app, "GET", "/openapi.json", "", None).await;
        let mut operations = 0;
        for (path, methods) in schema["paths"].as_object().unwrap() {
            for operation in methods.as_object().unwrap().values() {
                if operation["tags"][0] == "Downloads" {
                    operations += 1;
                    assert!(path.starts_with("/api/v1/"));
                    assert_eq!(
                        operation["security"][0]["personal_key"],
                        json!([])
                    );
                    assert!(operation["responses"]["401"].is_object());
                }
            }
        }
        assert_eq!(operations, 8);
        let disabled = crate::api::router(
            &crate::config::AuthConfig::default(),
            &crate::config::DocsConfig::default(),
            &f.pool,
            crate::jobs::ScanService::new(f.pool.clone(), vec![]),
        );
        let (status, _, error) = call(
            &disabled,
            "POST",
            "/api/v1/me/downloads/searches",
            &alice,
            Some(json!({"kind":"query","title":"X","artists":[]})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
        assert!(
            db::downloads::jobs(&f.pool, 1, 50, 0)
                .await
                .unwrap()
                .is_empty()
        );
        f.service.close().await;
        f.pool.close().await;
    }
}
