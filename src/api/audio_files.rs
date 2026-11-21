//! Recording file registration, stored metadata, and observed source tags.

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
    db::audio_files::{self, FileError},
    library::audio_files::{AudioFile, AudioProperties, NewAudioFile},
};

/// Register metadata operations using the existing recording path parameter.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(register_file, list_files))
        .routes(routes!(get_file))
        .routes(routes!(get_file_tags))
        .with_state(pool)
}

/// Declared file metadata; no bytes are uploaded and no file is accessed.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct RegisterFile {
    /// Absolute UTF-8 file path; up to 4096 bytes, no controls or parent dirs.
    #[schema(example = "/music/artist/track.flac")]
    path: String,
    /// ASCII format label; trimmed and lowercased, not inferred from content.
    #[schema(example = "flac")]
    format: String,
    /// Optional declared properties; omitted means all values are unknown.
    #[serde(default)]
    properties: AudioProperties,
}

/// Bounded metadata page for files attached to one recording.
#[derive(Serialize, ToSchema)]
struct FilePage {
    /// File metadata ordered by catalog identifier.
    items: Vec<AudioFile>,
    /// Maximum number of returned entries.
    limit: i64,
    /// Number of skipped entries in identifier order.
    offset: i64,
}

impl From<FileError> for ApiError {
    /// Preserve expected statuses without exposing SQL diagnostics.
    fn from(error: FileError) -> Self {
        match error {
            FileError::MissingRecording => Self::NotFound,
            FileError::PathConflict => {
                Self::Conflict("File path is registered")
            }
            FileError::Storage(error) => error.into(),
        }
    }
}

/// Register a file's declared metadata against a known recording.
///
/// This does not upload, read, move, or verify a file. Paths are normalized
/// lexically without resolving symlinks; repeated normalized paths return 409
/// even across recordings. Scanning and media probing are separate stages.
#[utoipa::path(
    post, path = "/recordings/{id}/files", tag = "audio_files",
    operation_id = "register_audio_file", request_body = RegisterFile,
    params(("id" = i64, Path, minimum = 1, description = "Recording ID")),
    responses(
        (status = 201, body = AudioFile, description = "Metadata registered",
            headers(("Location" = String, description = "Metadata URL"))),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn register_file(
    State(pool): State<SqlitePool>,
    OriginalUri(uri): OriginalUri,
    recording_id: Result<Path<i64>, PathRejection>,
    payload: Result<Json<RegisterFile>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(recording_id) = recording_id
        .map_err(|_| ApiError::BadRequest("Invalid recording id"))?;
    let Json(payload) = payload.map_err(ApiError::from)?;
    let input =
        NewAudioFile::new(&payload.path, &payload.format, payload.properties)
            .map_err(ApiError::Validation)?;
    let file = audio_files::create(&pool, recording_id, input).await?;
    let location = format!("{}/{}", uri.path(), file.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(file),
    )
        .into_response())
}

/// Retrieve metadata for a file belonging to the specified recording.
#[utoipa::path(
    get, path = "/recordings/{id}/files/{file_id}", tag = "audio_files",
    operation_id = "get_audio_file",
    params(
        ("id" = i64, Path, minimum = 1, description = "Recording ID"),
        ("file_id" = i64, Path, minimum = 1, description = "File ID")
    ),
    responses(
        (status = 200, body = AudioFile, description = "File metadata"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn get_file(
    State(pool): State<SqlitePool>,
    ids: Result<Path<(i64, i64)>, PathRejection>,
) -> Result<Json<AudioFile>, ApiError> {
    let Path((recording_id, file_id)) =
        ids.map_err(|_| ApiError::BadRequest("Invalid recording or file id"))?;
    Ok(Json(
        audio_files::find(&pool, recording_id, file_id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// Read observed source tags without interpreting them as catalog identities.
///
/// Returns 404 for unknown files, a wrong parent, or files never probed by a
/// background scan. The read-only CLI does not persist these observations.
#[utoipa::path(
    get, path = "/recordings/{id}/files/{file_id}/tags", tag = "audio_files",
    operation_id = "get_audio_file_tags",
    params(("id" = i64, Path, minimum = 1, description = "Recording ID"),
        ("file_id" = i64, Path, minimum = 1, description = "Audio file ID")),
    responses(
        (status = 200, body = crate::library::metadata::AudioTags,
            description = "Latest observed source tags"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn get_file_tags(
    State(pool): State<SqlitePool>,
    ids: Result<Path<(i64, i64)>, PathRejection>,
) -> Result<Json<crate::library::metadata::AudioTags>, ApiError> {
    let Path((recording_id, file_id)) =
        ids.map_err(|_| ApiError::BadRequest("Invalid recording or file id"))?;
    Ok(Json(
        crate::db::indexing::tags(&pool, recording_id, file_id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// List stored file metadata for a recording in ascending identifier order.
#[utoipa::path(
    get, path = "/recordings/{id}/files", tag = "audio_files",
    operation_id = "list_audio_files",
    params(("id" = i64, Path, minimum = 1, description = "Recording ID"),
        Pagination),
    responses(
        (status = 200, body = FilePage, description = "File metadata page"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn list_files(
    State(pool): State<SqlitePool>,
    recording_id: Result<Path<i64>, PathRejection>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<FilePage>, ApiError> {
    let Path(recording_id) = recording_id
        .map_err(|_| ApiError::BadRequest("Invalid recording id"))?;
    let Query(query) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    query.validate()?;
    let items =
        audio_files::list(&pool, recording_id, query.limit, query.offset)
            .await?;
    Ok(Json(FilePage {
        items,
        limit: query.limit,
        offset: query.offset,
    }))
}

#[cfg(test)]
mod tests {
    //! Metadata registration contracts against isolated migrated databases.

    use crate::{config::DocsConfig, db, library::recordings::NewRecording};
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;

    /// Seed one recording without requiring any physical audio file.
    async fn setup() -> (TempDir, SqlitePool, Router, i64) {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let recording = db::recordings::create(
            &pool,
            NewRecording::new("Recording", vec![]).unwrap(),
        )
        .await
        .unwrap();
        let app = crate::api::router(
            &DocsConfig::default(),
            &pool,
            crate::jobs::ScanService::new(pool.clone(), vec![]),
        );
        (directory, pool, app, recording.id)
    }

    /// Send optional JSON through the router and decode its metadata response.
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

    /// Metadata and optional quality values round-trip without file creation.
    #[tokio::test]
    async fn registers_and_reads_metadata_without_touching_files() {
        let (directory, pool, app, recording) = setup().await;
        let path = format!("/api/v1/recordings/{recording}/files");
        let physical = directory.path().join("song.flac");
        let (status, headers, file) = request(
            &app,
            "POST",
            &path,
            Some(json!({"path": physical, "format": " FLAC ",
                "properties": {"duration_ms": 1234, "sample_rate_hz": 44100,
                    "channels": 2, "bit_depth": 16}})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(file["recording_id"], recording);
        assert_eq!(file["format"], "flac");
        assert_eq!(file["properties"]["duration_ms"], 1234);
        assert!(file["properties"]["size_bytes"].is_null());
        assert!(!physical.exists());
        let location = headers[header::LOCATION].to_str().unwrap();
        assert_eq!(location, format!("{path}/{}", file["id"]));
        let (status, _, fetched) = request(&app, "GET", location, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched, file);
        pool.close().await;
    }

    /// Formats may share a recording; normalized paths stay globally unique.
    #[tokio::test]
    async fn supports_multiple_formats_and_global_path_conflicts() {
        let (_directory, pool, app, recording) = setup().await;
        let path = format!("/api/v1/recordings/{recording}/files");
        let (_, _, empty) = request(&app, "GET", &path, None).await;
        assert_eq!(empty, json!({"items": [], "limit": 50, "offset": 0}));
        let mut registered = Vec::new();
        for format in ["flac", "mp3"] {
            let (status, _, file) = request(
                &app,
                "POST",
                &path,
                Some(json!({
                    "path": format!("/music/song.{format}"), "format": format,
                })),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
            assert!(file["properties"]["bitrate_bps"].is_null());
            registered.push(file);
        }
        let (_, _, page) =
            request(&app, "GET", &format!("{path}?limit=1&offset=1"), None)
                .await;
        assert_eq!(page["items"], json!([registered[1]]));
        let (_, _, end) =
            request(&app, "GET", &format!("{path}?offset=2"), None).await;
        assert_eq!(end["items"], json!([]));
        let other = db::recordings::create(
            &pool,
            NewRecording::new("Other", vec![]).unwrap(),
        )
        .await
        .unwrap();
        let other_path = format!("/api/v1/recordings/{}/files", other.id);
        for target in [&path, &other_path] {
            let (status, _, error) = request(
                &app,
                "POST",
                target,
                Some(json!({
                    "path": "/music//./song.flac", "format": "flac",
                })),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(error["error"]["code"], "conflict");
        }
        let scoped = format!("{other_path}/{}", registered[0]["id"]);
        let (status, _, _) = request(&app, "GET", &scoped, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        pool.close().await;
    }

    /// Invalid paths, labels, and properties leave no catalog entries.
    #[tokio::test]
    async fn rejects_invalid_metadata_and_unknown_recordings() {
        let (_directory, pool, app, recording) = setup().await;
        let path = format!("/api/v1/recordings/{recording}/files");
        for payload in [
            json!({}),
            json!({"path": "a.flac", "format": "flac"}),
            json!({"path": "/music/../a.flac", "format": "flac"}),
            json!({"path": "/music/a.flac", "format": "audio/flac"}),
            json!({"path": "/music/a.flac", "format": "flac", "extra": 1}),
            json!({"path": "/music/a.flac", "format": "flac",
                "properties": {"duration_ms": 0}}),
            json!({"path": "/music/a.flac", "format": "flac",
                "properties": {"channels": "two"}}),
            json!({"path": "/music/a.flac", "format": "flac",
                "properties": {"unknown": 1}}),
        ] {
            let (status, _, error) =
                request(&app, "POST", &path, Some(payload)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(error["error"]["code"], "validation_error");
        }
        let missing = format!("/api/v1/recordings/{}/files", recording + 1);
        for (method, body) in [
            ("GET", None),
            (
                "POST",
                Some(json!({
                    "path": "/music/a.flac", "format": "flac",
                })),
            ),
        ] {
            let (status, _, error) =
                request(&app, method, &missing, body).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(error["error"]["code"], "not_found");
        }
        let (_, _, empty) = request(&app, "GET", &path, None).await;
        assert_eq!(empty["items"], json!([]));
        pool.close().await;
    }

    /// Query and identifier failures remain JSON errors with stable statuses.
    #[tokio::test]
    async fn validates_pagination_and_scoped_identifiers() {
        let (_directory, pool, app, recording) = setup().await;
        let path = format!("/api/v1/recordings/{recording}/files");
        for query in ["limit=0", "limit=101", "offset=-1", "unknown=1"] {
            let (status, _, error) =
                request(&app, "GET", &format!("{path}?{query}"), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["error"]["code"], "bad_request");
        }
        for invalid in
            ["/api/v1/recordings/abc/files".into(), format!("{path}/abc")]
        {
            let (status, _, _) = request(&app, "GET", &invalid, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        let (status, _, _) =
            request(&app, "GET", &format!("{path}/1"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        pool.close().await;
    }

    /// Media, JSON syntax, and SQL failures never expose raw internals.
    #[tokio::test]
    async fn extraction_and_storage_errors_use_safe_json() {
        let (_directory, pool, app, recording) = setup().await;
        let path = format!("/api/v1/recordings/{recording}/files");
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
