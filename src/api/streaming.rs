//! Authenticated, bounded-memory delivery of registered local audio files.

use std::{
    io::SeekFrom,
    path::{Path as FilePath, PathBuf},
    sync::Arc,
};

use super::error::{ApiError, ErrorResponse};
use crate::{audio, auth::CurrentUser, db};
use axum::{
    body::Body,
    extract::{FromRef, Path, State, rejection::PathRejection},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use sqlx::SqlitePool;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Register file streaming with the same canonical roots used by the scanner.
pub(super) fn router(
    pool: SqlitePool,
    roots: Arc<Vec<PathBuf>>,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(stream_audio))
        .with_state(StreamState { pool, roots })
}

/// Shared storage and configured filesystem boundaries for media access.
#[derive(Clone)]
struct StreamState {
    /// Catalog lookup and credential authentication.
    pool: SqlitePool,
    /// Canonical collection roots prepared before serving HTTP.
    roots: Arc<Vec<PathBuf>>,
}

impl FromRef<StreamState> for SqlitePool {
    /// Let the shared authentication extractor use the streaming database.
    fn from_ref(state: &StreamState) -> Self {
        state.pool.clone()
    }
}

/// OpenAPI-only model of the raw media body, independent of its container.
struct AudioBytes;

impl utoipa::PartialSchema for AudioBytes {
    /// Describe wire bytes as a binary string, without a JSON array envelope.
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

impl utoipa::ToSchema for AudioBytes {}

/// Stream the original bytes; requires any active key or password session.
/// Supports one byte range. Unsupported units/multiple ranges are ignored.
/// HEAD returns full-file headers without reading the body and ignores Range.
/// If-Range falls back to the full response; validators are not issued yet.
#[utoipa::path(get,
    path = "/recordings/{id}/files/{file_id}/stream", tag = "audio_files",
    operation_id = "stream_audio_file", security(("personal_key" = [])),
    params(
        ("id" = i64, Path, minimum = 1, description = "Recording ID"),
        ("file_id" = i64, Path, minimum = 1, description = "Audio file ID"),
        ("Range" = Option<String>, Header,
            description = "Single byte range: bytes=0-1023 or bytes=-512"),
        ("If-Range" = Option<String>, Header,
            description = "Causes a full response; no validators are issued")
    ), responses(
        (status = 200, description = "Original file; HEAD has no body",
            content((AudioBytes = "audio/*"),
                (AudioBytes = "application/octet-stream")),
            headers(
                ("Accept-Ranges" = String, description = "bytes"),
                ("Content-Length" = u64, description = "Current file size"))),
        (status = 206, description = "Selected original bytes",
            content((AudioBytes = "audio/*"),
                (AudioBytes = "application/octet-stream")),
            headers(
                ("Accept-Ranges" = String, description = "bytes"),
                ("Content-Length" = u64, description = "Selected byte count"),
                ("Content-Range" = String,
                    description = "bytes first-last/size"))),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 416, description = "Invalid or unsatisfiable byte range",
            body = ErrorResponse,
            headers(("Content-Range" = String, description = "bytes */size"))),
        (status = 500, response = ErrorResponse)
    ))]
async fn stream_audio(
    _user: CurrentUser,
    State(state): State<StreamState>,
    ids: Result<Path<(i64, i64)>, PathRejection>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let Path((recording_id, file_id)) =
        ids.map_err(|_| ApiError::BadRequest("Invalid recording or file ID"))?;
    if recording_id <= 0 || file_id <= 0 {
        return Err(ApiError::BadRequest("Identifiers must be positive"));
    }
    let metadata = db::audio_files::find(&state.pool, recording_id, file_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let opened = tokio::task::spawn_blocking(move || {
        audio::open(&state.roots, FilePath::new(&metadata.path))
    })
    .await
    .map_err(anyhow::Error::from)?
    .map_err(|error| match error {
        audio::OpenError::Unavailable => ApiError::NotFound,
        audio::OpenError::Io(error) => ApiError::Internal(error.into()),
    })?;
    let range = if method == Method::HEAD {
        Ok(None)
    } else {
        requested_range(&headers, opened.size)
    };
    let range = match range {
        Ok(range) => range,
        Err(()) => return range_error(opened.size),
    };
    let mut response = Response::builder()
        .status(if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, audio::content_type(&metadata.format))
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    let (start, length) = match range {
        Some(range) => {
            response = response.header(
                header::CONTENT_RANGE,
                format!(
                    "bytes {}-{}/{}",
                    range.start,
                    range.start + range.length - 1,
                    opened.size
                ),
            );
            (range.start, range.length)
        }
        None => (0, opened.size),
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let mut file = tokio::fs::File::from_std(opened.file);
        if start > 0 {
            file.seek(SeekFrom::Start(start))
                .await
                .map_err(anyhow::Error::from)?;
        }
        Body::from_stream(ReaderStream::with_capacity(
            file.take(length),
            64 * 1024,
        ))
    };
    response
        .header(header::CONTENT_LENGTH, length)
        .body(body)
        .map_err(|error| ApiError::Internal(error.into()))
}

/// A nonempty byte interval whose endpoint is bounded by the physical file.
#[derive(Debug, PartialEq, Eq)]
struct ByteRange {
    /// First selected byte offset.
    start: u64,
    /// Selected byte count.
    length: u64,
}

/// Ignore unsupported range forms and conditional ranges without a validator.
fn requested_range(
    headers: &HeaderMap,
    size: u64,
) -> Result<Option<ByteRange>, ()> {
    if headers.contains_key(header::IF_RANGE) {
        return Ok(None);
    }
    let mut values = headers.get_all(header::RANGE).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Ok(None);
    }
    let value = value.to_str().map_err(|_| ())?;
    let Some((unit, range)) = value.trim().split_once('=') else {
        return Err(());
    };
    if !unit.eq_ignore_ascii_case("bytes") || range.contains(',') {
        return Ok(None);
    }
    byte_range(range.trim(), size).map(Some)
}

/// Parse a single closed/open-ended/suffix range without integer overflow.
fn byte_range(value: &str, size: u64) -> Result<ByteRange, ()> {
    if size == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = decimal(end)?;
        if suffix == 0 {
            return Err(());
        }
        let length = suffix.min(size);
        return Ok(ByteRange {
            start: size - length,
            length,
        });
    }
    let start = decimal(start)?;
    let end = if end.is_empty() {
        size - 1
    } else {
        decimal(end)?.min(size - 1)
    };
    if start >= size || end < start {
        return Err(());
    }
    Ok(ByteRange {
        start,
        length: end - start + 1,
    })
}

/// Saturate arbitrarily large valid numerals instead of wrapping offsets.
fn decimal(value: &str) -> Result<u64, ()> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    Ok(value.bytes().fold(0_u64, |n, b| {
        n.saturating_mul(10).saturating_add(u64::from(b - b'0'))
    }))
}

/// Retain the shared JSON error envelope while reporting the current length.
fn range_error(size: u64) -> Result<Response, ApiError> {
    let mut response = ApiError::RangeNotSatisfiable.into_response();
    response.headers_mut().insert(
        header::CONTENT_RANGE,
        format!("bytes */{size}")
            .parse()
            .map_err(anyhow::Error::from)?,
    );
    response.headers_mut().insert(
        header::ACCEPT_RANGES,
        axum::http::HeaderValue::from_static("bytes"),
    );
    Ok(response)
}

#[cfg(test)]
mod tests {
    //! Ranges, authorization, physical files and binary HTTP responses.

    use super::*;
    use crate::library::{
        audio_files::{AudioProperties, NewAudioFile},
        recordings::NewRecording,
    };
    use axum::{Router, body::to_bytes, http::Request};
    use serde_json::Value;
    use std::{fs, os::unix::fs::symlink};
    use tower::ServiceExt;

    /// Exercise arithmetic boundaries, including numerals beyond u64.
    #[test]
    fn selects_closed_open_and_suffix_ranges_without_overflow() {
        for (value, start, length) in [
            ("0-0", 0, 1),
            ("2-5", 2, 4),
            ("6-", 6, 4),
            ("-3", 7, 3),
            ("-20", 0, 10),
            ("0-999", 0, 10),
            ("0-99999999999999999999999999999", 0, 10),
            ("-99999999999999999999999999999", 0, 10),
        ] {
            assert_eq!(byte_range(value, 10), Ok(ByteRange { start, length }));
        }
        for value in [
            "",
            "-",
            "-0",
            "10-",
            "4-2",
            "a-5",
            "2-x",
            "1--2",
            "99999999999999999999999-",
            "+1-3",
            " 1-3",
        ] {
            assert!(byte_range(value, 10).is_err(), "{value}");
        }
        assert!(byte_range("0-", 0).is_err());
        assert_eq!(
            byte_range("0-", u64::MAX),
            Ok(ByteRange {
                start: 0,
                length: u64::MAX
            })
        );
        assert_eq!(
            byte_range("18446744073709551614-", u64::MAX),
            Ok(ByteRange {
                start: u64::MAX - 1,
                length: 1
            })
        );
    }

    /// Unsupported and unverifiable ranges receive full representations.
    #[test]
    fn ignores_multiple_units_and_unverifiable_conditional_ranges() {
        for value in ["items=0-1", "bytes=0-1,4-5"] {
            let mut headers = HeaderMap::new();
            headers.insert(header::RANGE, value.parse().unwrap());
            assert_eq!(requested_range(&headers, 10), Ok(None));
        }
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=0-1".parse().unwrap());
        headers.append(header::RANGE, "bytes=2-3".parse().unwrap());
        assert_eq!(requested_range(&headers, 10), Ok(None));
        headers.remove(header::RANGE);
        headers.insert(header::RANGE, "bytes=99-".parse().unwrap());
        headers.insert(header::IF_RANGE, "\"unverified\"".parse().unwrap());
        assert_eq!(requested_range(&headers, 10), Ok(None));
    }

    /// One migrated catalog, an ordinary user and physical source data.
    struct Fixture {
        /// Keep the source and SQLite directory alive until test completion.
        _directory: tempfile::TempDir,
        /// Shared catalog and authentication storage.
        pool: SqlitePool,
        /// Canonical configured music directory.
        root: PathBuf,
        /// Registered source file's current physical path.
        path: PathBuf,
        /// Ordinary member credential, proving admin is not required.
        token: String,
        /// Production router using the fixture's allowed collection root.
        app: Router,
    }

    impl Fixture {
        /// Register stale size metadata so streaming must inspect its handle.
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("music");
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let path = root.join("source.flac");
            fs::write(&path, b"0123456789").unwrap();
            let pool = db::connect(&directory.path().join("catalog.db"))
                .await
                .unwrap();
            let recording = NewRecording::new("Source", vec![]).unwrap();
            db::recordings::create(&pool, recording).await.unwrap();
            let input = NewAudioFile::new(
                path.to_str().unwrap(),
                "flac",
                AudioProperties {
                    size_bytes: Some(999),
                    ..AudioProperties::default()
                },
            )
            .unwrap();
            db::audio_files::create(&pool, 1, input).await.unwrap();
            let token = crate::auth::token().unwrap();
            db::users::create(&pool, "member", &crate::auth::digest(&token))
                .await
                .unwrap();
            let app = Self::router(&pool, vec![root.clone()]);
            Self {
                _directory: directory,
                pool,
                root,
                path,
                token,
                app,
            }
        }

        /// Assemble the full API with the requested root policy.
        fn router(pool: &SqlitePool, roots: Vec<PathBuf>) -> Router {
            crate::api::router(
                &crate::config::AuthConfig::default(),
                &crate::config::DocsConfig { enabled: true },
                pool,
                crate::jobs::ScanService::new(pool.clone(), roots),
            )
        }

        /// Send requests with optional credentials and range headers.
        async fn request(
            &self,
            method: &str,
            path: &str,
            token: Option<&str>,
            headers: &[(&str, &str)],
        ) -> Response {
            let mut request = Request::builder().method(method).uri(path);
            if let Some(token) = token {
                request = request
                    .header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            self.app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap()
        }
    }

    /// Full/partial delivery and HEAD report current bytes without mutations.
    #[tokio::test]
    async fn delivers_original_bytes_current_size_and_ranges_to_members() {
        let fixture = Fixture::new().await;
        let url = "/api/v1/recordings/1/files/1/stream";
        let before = fs::metadata(&fixture.path).unwrap().modified().unwrap();
        for (range, expected, content_range) in [
            (None, b"0123456789".as_slice(), None),
            (Some("bytes=2-5"), b"2345".as_slice(), Some("bytes 2-5/10")),
            (Some("bytes=7-"), b"789".as_slice(), Some("bytes 7-9/10")),
            (Some("bytes=-4"), b"6789".as_slice(), Some("bytes 6-9/10")),
            (
                Some("bytes=0-999"),
                b"0123456789".as_slice(),
                Some("bytes 0-9/10"),
            ),
        ] {
            let mut headers = vec![("Accept-Encoding", "gzip")];
            if let Some(range) = range {
                headers.push(("Range", range));
            }
            let response = fixture
                .request("GET", url, Some(&fixture.token), &headers)
                .await;
            assert_eq!(
                response.status(),
                if range.is_some() {
                    StatusCode::PARTIAL_CONTENT
                } else {
                    StatusCode::OK
                }
            );
            assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/flac");
            assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                expected.len().to_string()
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "private, no-store"
            );
            assert!(
                !response.headers().contains_key(header::CONTENT_ENCODING)
            );
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_RANGE)
                    .map(|h| h.to_str().unwrap()),
                content_range
            );
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap(),
                expected
            );
        }
        let response = fixture
            .request(
                "HEAD",
                url,
                Some(&fixture.token),
                &[("Range", "bytes=99-")],
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
        assert!(!response.headers().contains_key(header::CONTENT_RANGE));
        assert!(
            to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
        for headers in [
            vec![("Range", "bytes=1-2,4-5")],
            vec![("Range", "bytes=1-2"), ("If-Range", "\"old\"")],
        ] {
            let response = fixture
                .request("GET", url, Some(&fixture.token), &headers)
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap(),
                b"0123456789".as_slice()
            );
        }
        assert_eq!(fs::read(&fixture.path).unwrap(), b"0123456789");
        assert_eq!(
            fs::metadata(&fixture.path).unwrap().modified().unwrap(),
            before
        );
        let size: i64 = sqlx::query_scalar(
            "SELECT size_bytes FROM audio_files WHERE id = 1",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(size, 999);
        fixture.pool.close().await;
    }

    /// Bodies are polled in chunks and cannot include bytes appended later.
    #[tokio::test]
    async fn streams_incrementally_and_stops_at_the_measured_size() {
        use axum::body::HttpBody;
        use std::{future::poll_fn, io::Write};

        let fixture = Fixture::new().await;
        let size = 2 * 1024 * 1024;
        fs::write(&fixture.path, vec![b'x'; size]).unwrap();
        let response = fixture
            .request(
                "GET",
                "/api/v1/recordings/1/files/1/stream",
                Some(&fixture.token),
                &[],
            )
            .await;
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            size.to_string()
        );
        fs::OpenOptions::new()
            .append(true)
            .open(&fixture.path)
            .unwrap()
            .write_all(b"newly appended bytes")
            .unwrap();
        let mut body = Box::pin(response.into_body());
        let mut received = 0;
        let mut chunks = 0;
        while let Some(frame) =
            poll_fn(|cx| body.as_mut().poll_frame(cx)).await
        {
            let bytes = frame.unwrap().into_data().unwrap();
            assert!(bytes.iter().all(|b| *b == b'x'));
            assert!(bytes.len() < size);
            received += bytes.len();
            chunks += 1;
        }
        assert_eq!(received, size);
        assert!(chunks > 1);
        fixture.pool.close().await;
    }

    /// Password sessions use the same authenticated media route as keys.
    #[tokio::test]
    async fn accepts_sessions_and_rejects_them_after_revocation() {
        let fixture = Fixture::new().await;
        db::sessions::set_password(&fixture.pool, "member", "fixture-hash")
            .await
            .unwrap();
        let snapshot = db::sessions::credential(&fixture.pool, "member")
            .await
            .unwrap()
            .unwrap();
        let token = crate::auth::session_token().unwrap();
        let digest = crate::auth::digest(&token);
        db::sessions::create(&fixture.pool, &snapshot, &digest, 60)
            .await
            .unwrap()
            .unwrap();
        let url = "/api/v1/recordings/1/files/1/stream";
        let response = fixture.request("GET", url, Some(&token), &[]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap(),
            b"0123456789".as_slice()
        );
        db::sessions::revoke(&fixture.pool, &digest).await.unwrap();
        assert_eq!(
            fixture
                .request("GET", url, Some(&token), &[])
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        fixture.pool.close().await;
    }

    /// Validate credentials first, then IDs and ranges, with safe JSON errors.
    #[tokio::test]
    async fn rejects_unauthorized_missing_and_unsatisfiable_requests() {
        let fixture = Fixture::new().await;
        let url = "/api/v1/recordings/1/files/1/stream";
        for method in ["GET", "HEAD"] {
            let response = fixture
                .request(
                    method,
                    "/api/v1/recordings/invalid/files/invalid/stream",
                    None,
                    &[("Range", "bytes=invalid")],
                )
                .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        }
        for (path, status) in [
            (
                "/api/v1/recordings/0/files/1/stream",
                StatusCode::BAD_REQUEST,
            ),
            (
                "/api/v1/recordings/x/files/1/stream",
                StatusCode::BAD_REQUEST,
            ),
            ("/api/v1/recordings/2/files/1/stream", StatusCode::NOT_FOUND),
            (
                "/api/v1/recordings/1/files/99/stream",
                StatusCode::NOT_FOUND,
            ),
        ] {
            assert_eq!(
                fixture
                    .request("GET", path, Some(&fixture.token), &[])
                    .await
                    .status(),
                status
            );
        }
        for range in ["bytes=10-", "bytes=-0", "bytes=7-3", "bytes=no"] {
            let response = fixture
                .request("GET", url, Some(&fixture.token), &[("Range", range)])
                .await;
            assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
            assert_eq!(
                response.headers()[header::CONTENT_RANGE],
                "bytes */10"
            );
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error"]["code"], "range_not_satisfiable");
        }
        fs::write(&fixture.path, b"").unwrap();
        let response =
            fixture.request("GET", url, Some(&fixture.token), &[]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "0");
        assert!(
            to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
        let response = fixture
            .request(
                "GET",
                url,
                Some(&fixture.token),
                &[("Range", "bytes=0-")],
            )
            .await;
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */0");
        db::users::revoke(&fixture.pool, "member").await.unwrap();
        assert_eq!(
            fixture
                .request("GET", url, Some(&fixture.token), &[])
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        fixture.pool.close().await;
    }

    /// Media access respects roots, symlinks and the binary API contract.
    #[tokio::test]
    async fn enforces_root_and_symlink_boundaries_and_documents_binary_output()
    {
        let mut fixture = Fixture::new().await;
        let url = "/api/v1/recordings/1/files/1/stream";
        fixture.app = Fixture::router(&fixture.pool, vec![]);
        assert_eq!(
            fixture
                .request("GET", url, Some(&fixture.token), &[])
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        fixture.app =
            Fixture::router(&fixture.pool, vec![fixture.root.clone()]);
        let outside = fixture.root.parent().unwrap().join("secret");
        fs::write(&outside, b"private").unwrap();
        fs::remove_file(&fixture.path).unwrap();
        symlink(&outside, &fixture.path).unwrap();
        assert_eq!(
            fixture
                .request("GET", url, Some(&fixture.token), &[])
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        fs::remove_file(&fixture.path).unwrap();
        fs::create_dir(&fixture.path).unwrap();
        assert_eq!(
            fixture
                .request("GET", url, Some(&fixture.token), &[])
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        fs::remove_dir(&fixture.path).unwrap();
        assert_eq!(
            fixture
                .request("GET", url, Some(&fixture.token), &[])
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        sqlx::query("UPDATE audio_files SET path = ? WHERE id = 1")
            .bind(outside.to_str().unwrap())
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(
            fixture
                .request("GET", url, Some(&fixture.token), &[])
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        let response =
            fixture.request("GET", "/openapi.json", None, &[]).await;
        let schema: Value = serde_json::from_slice(
            &to_bytes(response.into_body(), 256 * 1024).await.unwrap(),
        )
        .unwrap();
        let path = "/api/v1/recordings/{id}/files/{file_id}/stream";
        let operation = &schema["paths"][path]["get"];
        let binary = &schema["components"]["schemas"]["AudioBytes"];
        assert_eq!(binary["type"], "string");
        assert_eq!(binary["format"], "binary");
        assert!(operation["security"][0]["personal_key"].is_array());
        assert!(
            operation["responses"]["200"]["content"]["audio/*"].is_object()
        );
        assert!(
            operation["responses"]["206"]["headers"]["Content-Range"]
                .is_object()
        );
        assert!(
            operation["responses"]["416"]["headers"]["Content-Range"]
                .is_object()
        );
        fixture.pool.close().await;
    }
}
