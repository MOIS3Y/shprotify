//! Administrative metadata workflow and public validated cover delivery.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    auth::{Admin, CurrentUser},
    db::metadata::{self, Candidate, MetadataError, MetadataJob},
    jobs::metadata::{JobRequest, MetadataService, StartError},
    library::{
        artwork,
        enrichment::{ApplyCandidate, Correction, EntityKind, Metadata},
    },
};
use axum::{
    Json,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Bound concurrent upload decoders, including cancelled HTTP requests.
static ARTWORK_WORKERS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(2);

/// OpenAPI-only binary image payload, not a JSON string or byte array.
struct ImageBytes;

impl utoipa::PartialSchema for ImageBytes {
    /// Describe encoded JPEG/PNG bytes on the wire.
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::Schema> {
        use utoipa::openapi::schema::{
            KnownFormat, ObjectBuilder, SchemaFormat, Type,
        };
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
            .into()
    }
}
impl utoipa::ToSchema for ImageBytes {}

/// Shared storage and independently supervised background worker.
#[derive(Clone)]
struct MetadataState {
    /// Catalog pool.
    pool: SqlitePool,
    /// Adapter executor; lifetime is managed by application startup/shutdown.
    service: MetadataService,
}

impl axum::extract::FromRef<MetadataState> for SqlitePool {
    /// Auth extractors use the same pool as catalog mutations.
    fn from_ref(state: &MetadataState) -> Self {
        state.pool.clone()
    }
}

/// Mount fixed workflow paths before the entity-kind paths.
pub(super) fn router(
    pool: SqlitePool,
    service: MetadataService,
) -> OpenApiRouter {
    let json = OpenApiRouter::new()
        .routes(routes!(get_metadata, correct_metadata))
        .routes(routes!(get_revisions))
        .routes(routes!(start_job))
        .routes(routes!(get_job))
        .routes(routes!(list_jobs))
        .routes(routes!(correct_link))
        .routes(routes!(apply_candidate, reject_candidate))
        .routes(routes!(preview_artwork))
        .layer(DefaultBodyLimit::max(16 * 1024));
    let images = OpenApiRouter::new()
        .routes(routes!(put_artwork, delete_artwork))
        .routes(routes!(get_artwork))
        .layer(DefaultBodyLimit::max(artwork::MAX_BYTES));
    json.merge(images)
        .with_state(MetadataState { pool, service })
}

/// Version precondition for raw image upload/removal.
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
struct Version {
    /// Last observed positive metadata version.
    #[param(minimum = 1)]
    version: i64,
}

/// Administrative job snapshot and its completed suggestions.
#[derive(Serialize, ToSchema)]
struct JobDetails {
    /// Persisted lifecycle; completed may have zero candidates.
    job: MetadataJob,
    /// Pending/applied/rejected suggestions, excluding binary image bytes.
    candidates: Vec<Candidate>,
}

/// Normalize path extraction errors and disallow nonpositive IDs.
fn entity(
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
) -> Result<(EntityKind, i64), ApiError> {
    let Path((kind, id)) = path
        .map_err(|_| ApiError::BadRequest("Invalid metadata entity path"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("ID must be positive"));
    }
    Ok((kind, id))
}

/// Validate a standalone job/candidate identifier.
fn identifier(
    path: Result<Path<i64>, PathRejection>,
) -> Result<i64, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::BadRequest("Invalid ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("ID must be positive"));
    }
    Ok(id)
}

/// Map expected workflow failures while reserving SQL diagnostics for logs.
impl From<MetadataError> for ApiError {
    /// Preserve stable HTTP errors while keeping storage details internal.
    fn from(error: MetadataError) -> Self {
        match error {
            MetadataError::NotFound => Self::NotFound,
            MetadataError::Conflict => Self::Conflict(
                "Metadata changed or candidate already decided; reload",
            ),
            MetadataError::Invalid(message) => Self::Validation(message),
            MetadataError::Storage(error) => error.into(),
        }
    }
}

/// Administrative representations may contain provenance and search history.
fn private(value: impl IntoResponse) -> Response {
    let mut response = value.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

/// Return a binary cover with content-derived type, validators and no
/// sniffing.
fn image(
    art: metadata::StoredArtwork,
    headers: &HeaderMap,
    cache: &'static str,
) -> Response {
    let tag = format!("\"{}\"", art.digest);
    let unchanged = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|v| v.trim() == "*" || v.trim().trim_start_matches("W/") == tag);
    let mut response = if unchanged {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        ([(header::CONTENT_TYPE, art.mime)], art.data).into_response()
    };
    if let Ok(tag) = HeaderValue::from_str(&tag) {
        response.headers_mut().insert(header::ETAG, tag);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// Read editable fields, external identity and the current version.
#[utoipa::path(get, path = "/metadata/{kind}/{id}", tag = "metadata",
    operation_id = "get_metadata",
    params(("kind" = EntityKind, Path),("id" = i64, Path, minimum = 1)),
    security(("personal_key" = [])),
    responses((status = 200, description = "Operation result",
            body = Metadata),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_metadata(
    _admin: Admin,
    State(state): State<MetadataState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    Ok(private(Json(metadata::get(&state.pool, kind, id).await?)))
}

/// Apply an explicit full correction without touching source files.
#[utoipa::path(put, path = "/metadata/{kind}/{id}", tag = "metadata",
    operation_id = "correct_metadata",
    params(("kind" = EntityKind, Path),("id" = i64, Path, minimum = 1)),
    request_body = Correction,
    security(("personal_key" = [])),
    responses((status = 200, description = "Operation result",
            body = Metadata),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn correct_metadata(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<MetadataState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
    input: Result<Json<Correction>, JsonRejection>,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    let Json(mut input) = input?;
    input.validate(kind).map_err(ApiError::Validation)?;
    Ok(private(Json(
        metadata::correct(&state.pool, kind, id, input, user.id).await?,
    )))
}

/// Read a bounded audit page, newest first.
#[utoipa::path(get, path = "/metadata/{kind}/{id}/revisions", tag = "metadata",
    operation_id = "get_revisions",
    params(("kind" = EntityKind, Path),("id" = i64, Path, minimum = 1),
        Pagination),
    security(("personal_key" = [])),
    responses((status = 200, description = "Operation result",
            body = serde_json::Value),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_revisions(
    _admin: Admin,
    State(state): State<MetadataState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    metadata::get(&state.pool, kind, id).await?;
    let Query(page) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    page.validate()?;
    let items =
        metadata::revisions(&state.pool, kind, id, page.limit, page.offset)
            .await?;
    Ok(private(Json(serde_json::json!({"items":items,
        "limit":page.limit,"offset":page.offset}))))
}

/// Start one bounded background lookup/extraction; catalog stays unchanged.
#[utoipa::path(post, path = "/metadata/{kind}/{id}/jobs", tag = "metadata",
    operation_id = "start_job",
    params(("kind" = EntityKind, Path),("id" = i64, Path, minimum = 1)),
    request_body = JobRequest,
    security(("personal_key" = [])),
    responses((status = 202, description = "Operation result",
            body = MetadataJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn start_job(
    _admin: Admin,
    State(state): State<MetadataState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
    input: Result<Json<JobRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    let Json(input) = input?;
    let job =
        state
            .service
            .start(kind, id, input)
            .await
            .map_err(|e| match e {
                StartError::Invalid(message) => ApiError::Validation(message),
                StartError::Busy => {
                    ApiError::Conflict("Metadata worker is busy")
                }
                StartError::Metadata(error) => error.into(),
                StartError::Storage(error) => error.into(),
            })?;
    Ok(private((
        StatusCode::ACCEPTED,
        [(
            header::LOCATION,
            format!("/api/v1/metadata/jobs/{}", job.id),
        )],
        Json(job),
    )))
}

/// Inspect a persisted job and review its candidates.
#[utoipa::path(get, path = "/metadata/jobs/{id}", tag = "metadata",
    operation_id = "get_job",
    params(("id" = i64, Path, minimum = 1)),
    security(("personal_key" = [])),
    responses((status = 200, description = "Operation result",
            body = JobDetails),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_job(
    _admin: Admin,
    State(state): State<MetadataState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    // A job may finish during polling; publish candidates only when terminal.
    let job = metadata::job(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let candidates = if job.status == "completed" {
        metadata::candidates(&state.pool, id).await?
    } else {
        vec![]
    };
    Ok(private(Json(JobDetails { job, candidates })))
}

/// Confirm identity and explicitly selected fields using the current version.
#[utoipa::path(post, path = "/metadata/candidates/{id}", tag = "metadata",
    operation_id = "apply_candidate",
    params(("id" = i64, Path, minimum = 1)),
    request_body = ApplyCandidate,
    security(("personal_key" = [])),
    responses((status = 200, description = "Operation result",
            body = Metadata),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn apply_candidate(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<MetadataState>,
    path: Result<Path<i64>, PathRejection>,
    input: Result<Json<ApplyCandidate>, JsonRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    let Json(input) = input?;
    if input.version <= 0 {
        return Err(ApiError::Validation("version must be positive"));
    }
    Ok(private(Json(
        metadata::apply(&state.pool, id, input, user.id).await?,
    )))
}

/// Reject a pending candidate without modifying the catalog.
#[utoipa::path(delete, path = "/metadata/candidates/{id}", tag = "metadata",
    operation_id = "reject_candidate",
    params(("id" = i64, Path, minimum = 1)),
    security(("personal_key" = [])),
    responses((status = 204, description = "Operation result"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn reject_candidate(
    _admin: Admin,
    State(state): State<MetadataState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    metadata::reject(&state.pool, id).await?;
    Ok(private(StatusCode::NO_CONTENT))
}

/// Upload validated JPEG/PNG bytes, maximum 5 MiB.
#[utoipa::path(put, path = "/metadata/{kind}/{id}/artwork", tag = "metadata",
    operation_id = "put_artwork",
    params(("kind" = EntityKind, Path),("id" = i64, Path, minimum = 1),
        Version),
    request_body(content = ImageBytes,
        content_type = "application/octet-stream"),
    security(("personal_key" = [])),
    responses((status = 200, description = "Operation result",
            body = Metadata),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn put_artwork(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<MetadataState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
    query: Result<Query<Version>, QueryRejection>,
    bytes: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    let Query(version) = query
        .map_err(|_| ApiError::BadRequest("A version query is required"))?;
    if version.version <= 0 {
        return Err(ApiError::BadRequest("version must be positive"));
    }
    let bytes = bytes.map_err(|error| {
        if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge
        } else {
            ApiError::BadRequest("Cannot read artwork body")
        }
    })?;
    let permit = ARTWORK_WORKERS
        .try_acquire()
        .map_err(|_| ApiError::Conflict("Artwork decoder is busy"))?;
    let art = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        artwork::validate(bytes.to_vec())
    })
    .await
    .map_err(anyhow::Error::from)?
    .map_err(|_| ApiError::Validation("Invalid or oversized JPEG/PNG"))?;
    Ok(private(Json(
        metadata::set_artwork(
            &state.pool,
            kind,
            id,
            version.version,
            Some(art),
            user.id,
        )
        .await?,
    )))
}

/// Remove a cover without deleting any catalog or music data.
#[utoipa::path(delete, path = "/metadata/{kind}/{id}/artwork",
    tag = "metadata",
    operation_id = "delete_artwork",
    params(("kind" = EntityKind, Path),("id" = i64, Path, minimum = 1),
        Version),
    security(("personal_key" = [])),
    responses((status = 204, description = "Operation result"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn delete_artwork(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<MetadataState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
    query: Result<Query<Version>, QueryRejection>,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    let Query(version) = query
        .map_err(|_| ApiError::BadRequest("A version query is required"))?;
    if version.version <= 0 {
        return Err(ApiError::BadRequest("version must be positive"));
    }
    metadata::set_artwork(
        &state.pool,
        kind,
        id,
        version.version,
        None,
        user.id,
    )
    .await?;
    Ok(private(StatusCode::NO_CONTENT))
}

/// Read a stored cover; catalog artwork is public like catalog metadata.
#[utoipa::path(get, path = "/artwork/{kind}/{id}", tag = "metadata",
    operation_id = "get_artwork",
    params(("kind" = EntityKind, Path),("id" = i64, Path, minimum = 1)),
    responses((status = 200, description = "Stored JPEG/PNG",
            content((ImageBytes = "image/png"), (ImageBytes = "image/jpeg"))),
        (status = 304, description = "Image matches If-None-Match"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_artwork(
    State(state): State<MetadataState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    let art = metadata::artwork(&state.pool, kind, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(image(art, &headers, "public, no-cache"))
}

/// Preview a validated suggested cover before accepting it.
#[utoipa::path(get, path = "/metadata/candidates/{id}/artwork",
    tag = "metadata",
    operation_id = "preview_artwork",
    params(("id" = i64, Path, minimum = 1)),
    security(("personal_key" = [])),
    responses((status = 200, description = "Stored JPEG/PNG",
            content((ImageBytes = "image/png"), (ImageBytes = "image/jpeg"))),
        (status = 304, description = "Image matches If-None-Match"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn preview_artwork(
    _admin: Admin,
    State(state): State<MetadataState>,
    path: Result<Path<i64>, PathRejection>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    let art = metadata::preview(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(image(art, &headers, "private, no-store"))
}

/// Correct a file/track match; this never merges recordings or personal data.
#[utoipa::path(put, path = "/metadata/links/{kind}/{id}", tag = "metadata",
    operation_id = "correct_metadata_link",
    params(("kind" = crate::library::enrichment::LinkKind, Path),
        ("id" = i64, Path, minimum = 1)),
    request_body = crate::library::enrichment::LinkCorrection,
    security(("personal_key" = [])), responses(
        (status = 204, description = "Association corrected"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn correct_link(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<MetadataState>,
    path: Result<
        Path<(crate::library::enrichment::LinkKind, i64)>,
        PathRejection,
    >,
    input: Result<
        Json<crate::library::enrichment::LinkCorrection>,
        JsonRejection,
    >,
) -> Result<Response, ApiError> {
    let Path((kind, id)) =
        path.map_err(|_| ApiError::BadRequest("Invalid association path"))?;
    let Json(input) = input?;
    if id <= 0 || input.recording_id <= 0 || input.expected_recording_id <= 0 {
        return Err(ApiError::Validation("IDs must be positive"));
    }
    metadata::relink(&state.pool, kind, id, input, user.id).await?;
    Ok(private(StatusCode::NO_CONTENT))
}

/// List persisted job history, newest first, without contacting providers.
#[utoipa::path(get, path = "/metadata/jobs", tag = "metadata",
    operation_id = "list_metadata_jobs", params(Pagination),
    security(("personal_key" = [])), responses(
        (status = 200, description = "Metadata job page",
            body = MetadataJobPage),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn list_jobs(
    _admin: Admin,
    State(state): State<MetadataState>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(page) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    page.validate()?;
    let items = metadata::jobs(&state.pool, page.limit, page.offset).await?;
    Ok(private(Json(MetadataJobPage {
        items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// Bounded persisted operation history; candidates are loaded per job.
#[derive(Serialize, ToSchema)]
struct MetadataJobPage {
    /// Jobs in descending ID order.
    items: Vec<MetadataJob>,
    /// Maximum jobs returned; default 50, range 1–100.
    limit: i64,
    /// Number skipped.
    offset: i64,
}

#[cfg(test)]
mod tests {
    //! Authenticated edit, image delivery and strict request validation.
    use super::*;
    use crate::db;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
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

    /// Test account identities, without any production authentication bypass.
    struct Fixture {
        /// Retain the file-backed SQLite directory until the test finishes.
        _directory: tempfile::TempDir,
        /// Shared catalog and personal activity database.
        pool: SqlitePool,
        /// Complete API with generated documentation enabled.
        app: Router,
        /// Administrator performing catalog corrections.
        alice: String,
        /// Ordinary member, used to verify administrative route protection.
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
            db::users::set_role(&pool, "alice", crate::auth::Role::Admin)
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

    /// An admin can correct metadata; every other role is rejected first.
    #[tokio::test]
    async fn exposes_versioned_edits_without_leaking_administrative_data() {
        let f = Fixture::new().await;
        let url = "/api/v1/metadata/recording/1";
        for (token, expected) in [(None, 401), (Some(f.bob.as_str()), 403)] {
            for (method, path) in [
                ("GET", url),
                ("PUT", url),
                ("POST", "/api/v1/metadata/recording/1/jobs"),
                ("GET", "/api/v1/metadata/jobs/1"),
                ("POST", "/api/v1/metadata/candidates/1"),
                ("DELETE", "/api/v1/metadata/candidates/1"),
                ("PUT", "/api/v1/metadata/recording/1/artwork"),
                ("DELETE", "/api/v1/metadata/recording/1/artwork"),
                ("GET", "/api/v1/metadata/candidates/1/artwork"),
                ("PUT", "/api/v1/metadata/links/file/1"),
            ] {
                assert_eq!(
                    request(
                        &f.app,
                        method,
                        path,
                        token,
                        Some(json!({"invalid":true}))
                    )
                    .await
                    .0,
                    expected,
                    "{method} {path}"
                );
            }
        }
        let original = request(&f.app, "GET", url, Some(&f.alice), None).await;
        assert_eq!(original.0, 200);
        assert_eq!(original.2["version"], 1);
        let input = json!({"version":1,"name":" Corrected ",
            "artist_ids":[],"mbid":null});
        let saved =
            request(&f.app, "PUT", url, Some(&f.alice), Some(input.clone()))
                .await;
        assert_eq!(saved.0, 200);
        assert_eq!(saved.2["name"], "Corrected");
        assert_eq!(saved.2["version"], 2);
        assert_eq!(saved.1[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(
            request(&f.app, "PUT", url, Some(&f.alice), Some(input))
                .await
                .0,
            409
        );
        let catalog =
            request(&f.app, "GET", "/api/v1/recordings/1", None, None)
                .await
                .2;
        assert_eq!(catalog["title"], "Corrected");
        let search =
            request(&f.app, "GET", "/api/v1/search?q=Corrected", None, None)
                .await
                .2;
        assert_eq!(search["items"].as_array().unwrap().len(), 1);
        let revisions = request(
            &f.app,
            "GET",
            &format!("{url}/revisions"),
            Some(&f.alice),
            None,
        )
        .await
        .2;
        assert_eq!(revisions["items"].as_array().unwrap().len(), 2);
        assert_eq!(revisions["items"][1]["snapshot"]["name"], "Track");
        assert_eq!(
            request(
                &f.app,
                "POST",
                &format!("{url}/jobs"),
                Some(&f.alice),
                Some(json!({"action":"search"}))
            )
            .await
            .0,
            422
        );
        for body in [
            json!({"version":2,"name":"X","artist_ids":[999]}),
            json!({"version":2,"name":"X","artist_ids":[],"mbid":"bad"}),
            json!({"version":2,"name":"X","artist_ids":[],"extra":1}),
        ] {
            assert_eq!(
                request(&f.app, "PUT", url, Some(&f.alice), Some(body))
                    .await
                    .0,
                422
            );
        }
        let schema =
            request(&f.app, "GET", "/openapi.json", None, None).await.2;
        let responses = &schema["paths"]["/api/v1/artwork/{kind}/{id}"]["get"]
            ["responses"];
        let image = &responses["200"]["content"];
        assert!(image["image/png"].is_object());
        assert!(image["image/jpeg"].is_object());
        f.pool.close().await;
    }

    /// Covers are validated on upload, conditional on GET, and removed by CAS.
    #[tokio::test]
    async fn uploads_serves_and_removes_validated_artwork() {
        let f = Fixture::new().await;
        let url = "/api/v1/metadata/recording/1/artwork?version=1";
        let png = artwork::tests::png();
        let request = Request::builder()
            .method("PUT")
            .uri(url)
            .header(header::AUTHORIZATION, format!("Bearer {}", f.alice))
            .header(header::CONTENT_TYPE, "image/png")
            .body(Body::from(png.clone()))
            .unwrap();
        let response = f.app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let public = "/api/v1/artwork/recording/1";
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder().uri(public).body(Body::empty()).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        let tag = response.headers()[header::ETAG].clone();
        assert_eq!(
            to_bytes(response.into_body(), artwork::MAX_BYTES)
                .await
                .unwrap()
                .as_ref(),
            png
        );
        let response = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(public)
                    .header(header::IF_NONE_MATCH, tag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            super::tests::request(&f.app, "DELETE", url, Some(&f.alice), None)
                .await
                .0,
            409
        );
        let cleared = super::tests::request(
            &f.app,
            "DELETE",
            "/api/v1/metadata/recording/1/artwork?version=2",
            Some(&f.alice),
            None,
        )
        .await;
        assert_eq!(cleared.0, 204);
        assert_eq!(
            super::tests::request(&f.app, "GET", public, None, None)
                .await
                .0,
            404
        );
        f.pool.close().await;
    }
}
