//! Artist HTTP operations, input validation, and OpenAPI contracts.

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
    library::{Artist, ArtistName},
};

use crate::library::browse::{ArtistQuery, ArtistSummary};

use super::error::{ApiError, ErrorResponse};

/// Register version-local artist routes with their database state.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_artist, list_artists))
        .routes(routes!(get_artist))
        .with_state(pool)
}

/// Request to add an artist; matching names do not imply the same identity.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CreateArtist {
    /// Display name, trimmed before validation; 1–255 Unicode characters.
    #[schema(min_length = 1, max_length = 255, example = "Massive Attack")]
    name: String,
}

/// A bounded slice of the catalog with the effective paging parameters.
#[derive(Serialize, ToSchema)]
struct ArtistPage {
    /// Artists in the requested order; empty beyond the last page.
    items: Vec<ArtistSummary>,
    /// Maximum number of returned items.
    limit: i64,
    /// Total rows matching this filter in the same read snapshot.
    total: i64,
    /// Whether another page exists after the returned items.
    has_more: bool,
    /// Number of skipped artists.
    offset: i64,
}

/// Add an artist to the shared catalog.
///
/// Names are trimmed and may duplicate existing names. Control characters
/// are rejected.
///
/// Requires the administrator role.
#[utoipa::path(
    post, path = "/artists", tag = "Admin: Catalog",
    operation_id = "create_artist", request_body = CreateArtist,
    security(("personal_key" = [])),
    responses(
        (status = 201, body = Artist, description = "Artist created",
            headers(("Location" = String,
                description = "Artist resource URL"))),
        (status = 400, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn create_artist(
    _admin: crate::auth::Admin,
    State(pool): State<SqlitePool>,
    OriginalUri(uri): OriginalUri,
    payload: Result<Json<CreateArtist>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(payload) = payload.map_err(ApiError::from)?;
    let name = ArtistName::parse(&payload.name).map_err(|_| {
        ApiError::Validation(
            "Name must be 1–255 characters without control characters",
        )
    })?;
    let artist = db::artists::create(&pool, &name).await?;
    let location = format!("{}/{}", uri.path(), artist.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(artist),
    )
        .into_response())
}

/// Retrieve one catalog artist by its positive identifier.
#[utoipa::path(
    get, path = "/artists/{id}", tag = "Artists",
    operation_id = "get_artist",
    params(("id" = i64, Path, minimum = 1, description = "Artist identifier")),
    responses(
        (status = 200, body = ArtistSummary, description = "Artist found"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn get_artist(
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<ArtistSummary>, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid artist id"))?;
    let artist = db::browse::artist(&pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(artist))
}

/// Browse artist cards with ID/name sorting and snapshot-consistent totals.
#[utoipa::path(
    get, path = "/artists", tag = "Artists", operation_id = "list_artists",
    params(ArtistQuery),
    responses(
        (status = 200, body = ArtistPage, description = "Artist page"),
        (status = 400, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn list_artists(
    State(pool): State<SqlitePool>,
    query: Result<Query<ArtistQuery>, QueryRejection>,
) -> Result<Json<ArtistPage>, ApiError> {
    let Query(query) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    query.validate().map_err(ApiError::BadRequest)?;
    let (items, total) = db::browse::artists(&pool, &query).await?;
    Ok(Json(ArtistPage {
        has_more: query.offset.saturating_add(items.len() as i64) < total,
        total,
        items,
        limit: query.limit,
        offset: query.offset,
    }))
}

#[cfg(test)]
mod tests {
    //! Full artist HTTP contracts against isolated, migrated SQLite files.

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

    /// Keep the temporary directory alive for the lifetime of its pool.
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

    /// Execute one request through the production router and decode its JSON.
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

    /// Creation returns 201 and Location; fetching returns the stored artist.
    #[tokio::test]
    async fn creates_and_fetches_normalized_artist() {
        let (_directory, pool, app) = setup().await;
        let (status, headers, artist) = request(
            &app,
            "POST",
            "/api/v1/artists",
            Some(json!({"name": " Кино "})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(artist["name"], "Кино");
        assert!(artist["id"].as_i64().unwrap() > 0);
        let location = headers[header::LOCATION].to_str().unwrap();
        let (status, _, fetched) = request(&app, "GET", location, None).await;
        assert_eq!(status, StatusCode::OK);
        crate::api::tests::assert_base(&fetched, &artist);
        pool.close().await;
    }

    /// Listing is bounded, stable, and preserves distinct equal-name artists.
    #[tokio::test]
    async fn lists_artists_with_pagination_and_duplicate_names() {
        let (_directory, pool, app) = setup().await;
        let (_, _, empty) =
            request(&app, "GET", "/api/v1/artists", None).await;
        assert_eq!(
            empty,
            json!({"items": [], "limit": 50, "offset": 0,
            "total": 0,"has_more":false})
        );
        let mut ids = Vec::new();
        for name in ["Same", "Same", "Third"] {
            let (status, _, artist) = request(
                &app,
                "POST",
                "/api/v1/artists",
                Some(json!({"name": name})),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
            ids.push(artist["id"].as_i64().unwrap());
        }
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        let (_, _, page) =
            request(&app, "GET", "/api/v1/artists?limit=1&offset=1", None)
                .await;
        assert_eq!(
            page,
            json!({
                "items": [{"id": ids[1], "name": "Same",
                    "recording_count":0,"release_count":0,
                    "artwork_url":null}],
                "total":3,"has_more":true,
                "limit": 1, "offset": 1,
            })
        );
        let (_, _, end) =
            request(&app, "GET", "/api/v1/artists?offset=3", None).await;
        assert_eq!(end["items"], json!([]));
        pool.close().await;
    }

    /// Invalid fields leave the catalog unchanged and return JSON errors.
    #[tokio::test]
    async fn rejects_invalid_artist_fields() {
        let (_directory, pool, app) = setup().await;
        for body in [
            json!({"name": "  "}),
            json!({"name": "a\nb"}),
            json!({"name": "я".repeat(256)}),
            json!({}),
            json!({"name": 42}),
            json!({"name": "Valid", "extra": true}),
        ] {
            let (status, _, body) =
                request(&app, "POST", "/api/v1/artists", Some(body)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(body["error"]["code"], "validation_error");
        }
        let (_, _, page) = request(&app, "GET", "/api/v1/artists", None).await;
        assert_eq!(page["items"], json!([]));
        pool.close().await;
    }

    /// Syntax, media-type, and body-size failures retain their HTTP semantics.
    #[tokio::test]
    async fn extractor_rejections_use_shared_json_errors() {
        let (_directory, pool, app) = setup().await;
        for (body, content_type, status, code) in [
            ("{".into(), "application/json", 400, "bad_request"),
            ("{}".into(), "text/plain", 415, "unsupported_media_type"),
            (
                "x".repeat(3 * 1024 * 1024),
                "application/json",
                413,
                "payload_too_large",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/v1/artists")
                        .header(header::CONTENT_TYPE, content_type)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            let error: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(error["error"]["code"], code);
        }
        pool.close().await;
    }

    /// Invalid parameters return 400; absent artist identifiers return 404.
    #[tokio::test]
    async fn rejects_invalid_queries_and_reports_missing_artists() {
        let (_directory, pool, app) = setup().await;
        for path in [
            "/api/v1/artists?limit=0",
            "/api/v1/artists?limit=101",
            "/api/v1/artists?offset=-1",
            "/api/v1/artists?limit=abc",
            "/api/v1/artists?unknown=1",
            "/api/v1/artists/not-an-id",
            "/api/v1/artists/9223372036854775808",
        ] {
            let (status, _, body) = request(&app, "GET", path, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(body["error"]["code"], "bad_request");
        }
        for path in ["/api/v1/artists/1", "/api/v1/artists/0"] {
            let (status, _, body) = request(&app, "GET", path, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(body["error"]["code"], "not_found");
        }
        pool.close().await;
    }

    /// Unexpected storage errors never expose driver messages to the client.
    #[tokio::test]
    async fn storage_failure_returns_safe_internal_error() {
        let (_directory, pool, app) = setup().await;
        pool.close().await;
        let (status, _, body) =
            request(&app, "GET", "/api/v1/artists", None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({"error": {
                "code": "internal_error", "message": "Internal server error",
            }})
        );
    }
}
