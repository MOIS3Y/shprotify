//! Release HTTP contracts, validation, and atomic catalog operations.

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
    library::releases::{NewRelease, Release, ReleaseKind},
};

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};

/// Register version-local release operations with the shared SQLite pool.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_release, list_releases))
        .routes(routes!(get_release))
        .with_state(pool)
}

/// Release input; artist credits preserve caller-supplied order.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CreateRelease {
    /// Title trimmed before validating 1–255 characters without controls.
    #[schema(example = "Mezzanine")]
    title: String,
    /// Primary release classification.
    kind: ReleaseKind,
    /// Up to 100 distinct positive existing IDs; omitted means no credits.
    #[serde(default)]
    #[schema(max_items = 100)]
    artist_ids: Vec<i64>,
}

/// Bounded catalog slice in ascending release identifier order.
#[derive(Serialize, ToSchema)]
struct ReleasePage {
    /// Releases with complete ordered credits; empty beyond the last page.
    items: Vec<Release>,
    /// Effective maximum number of releases in this response.
    limit: i64,
    /// Number of skipped releases.
    offset: i64,
}

/// Create a release and its ordered artist credits atomically.
///
/// Titles may repeat. Empty credits support uncredited compilations. Unknown
/// artists cause 422 and leave no release or credit rows behind.
#[utoipa::path(
    post, path = "/releases", tag = "releases",
    operation_id = "create_release", request_body = CreateRelease,
    responses(
        (status = 201, body = Release, description = "Release created",
            headers(("Location" = String, description = "Release URL"))),
        (status = 400, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn create_release(
    State(pool): State<SqlitePool>,
    OriginalUri(uri): OriginalUri,
    payload: Result<Json<CreateRelease>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(payload) = payload.map_err(ApiError::from)?;
    let input =
        NewRelease::new(&payload.title, payload.kind, payload.artist_ids)
            .map_err(ApiError::Validation)?;
    let release = db::releases::create(&pool, input).await.map_err(
        |error| match error {
            db::releases::CreateReleaseError::UnknownArtist => {
                ApiError::Validation("A credited artist does not exist")
            }
            db::releases::CreateReleaseError::Storage(error) => error.into(),
        },
    )?;
    let location = format!("{}/{}", uri.path(), release.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(release),
    )
        .into_response())
}

/// Retrieve one release with credits in their original order.
#[utoipa::path(
    get, path = "/releases/{id}", tag = "releases",
    operation_id = "get_release",
    params(("id" = i64, Path, minimum = 1, description = "Release ID")),
    responses(
        (status = 200, body = Release, description = "Release found"),
        (status = 400, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn get_release(
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<Release>, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid release id"))?;
    Ok(Json(
        db::releases::find(&pool, id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// List whole releases, including all credits, in ascending ID order.
#[utoipa::path(
    get, path = "/releases", tag = "releases", operation_id = "list_releases",
    params(Pagination),
    responses(
        (status = 200, body = ReleasePage, description = "Release page"),
        (status = 400, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn list_releases(
    State(pool): State<SqlitePool>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<ReleasePage>, ApiError> {
    let Query(query) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    query.validate()?;
    let items = db::releases::list(&pool, query.limit, query.offset).await?;
    Ok(Json(ReleasePage {
        items,
        limit: query.limit,
        offset: query.offset,
    }))
}

#[cfg(test)]
mod tests {
    //! Migrated HTTP workflows, validation, and stable error envelopes.

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

    /// Build a full application router against an isolated database file.
    async fn setup() -> (TempDir, SqlitePool, Router) {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("library.db"))
            .await
            .unwrap();
        let router = crate::api::router(&DocsConfig::default(), &pool);
        (directory, pool, router)
    }

    /// Send JSON through the production router and decode the response.
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

    /// A release can be created, fetched by Location, and listed with credits.
    #[tokio::test]
    async fn creates_reads_and_lists_joint_releases() {
        let (_directory, pool, app) = setup().await;
        let mut artists = Vec::new();
        for name in ["First", "Second"] {
            let (_, _, artist) = request(
                &app,
                "POST",
                "/api/v1/artists",
                Some(json!({"name": name})),
            )
            .await;
            artists.push(artist["id"].as_i64().unwrap());
        }
        artists.reverse();
        let (status, headers, release) = request(
            &app,
            "POST",
            "/api/v1/releases",
            Some(json!({
                "title": " Совместный релиз ", "kind": "album",
                "artist_ids": artists,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(release["title"], "Совместный релиз");
        assert_eq!(release["artist_ids"], json!(artists));
        let location = headers[header::LOCATION].to_str().unwrap();
        let (status, _, fetched) = request(&app, "GET", location, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched, release);
        let (_, _, page) =
            request(&app, "GET", "/api/v1/releases?limit=1&offset=0", None)
                .await;
        assert_eq!(page, json!({"items": [release], "limit": 1, "offset": 0}));
        pool.close().await;
    }

    /// Uncredited compilations and all supported release kinds round-trip.
    #[tokio::test]
    async fn supports_release_kinds_and_bounded_pages() {
        let (_directory, pool, app) = setup().await;
        let (_, _, page) =
            request(&app, "GET", "/api/v1/releases", None).await;
        assert_eq!(page, json!({"items": [], "limit": 50, "offset": 0}));
        for kind in ["album", "single", "ep", "compilation", "other"] {
            let (status, _, release) = request(
                &app,
                "POST",
                "/api/v1/releases",
                Some(json!({"title": "Same", "kind": kind})),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
            assert_eq!(release["kind"], kind);
            assert_eq!(release["artist_ids"], json!([]));
        }
        let (_, _, page) =
            request(&app, "GET", "/api/v1/releases?limit=1&offset=3", None)
                .await;
        assert_eq!(page["items"][0]["kind"], "compilation");
        let (_, _, end) =
            request(&app, "GET", "/api/v1/releases?offset=5", None).await;
        assert_eq!(end["items"], json!([]));
        pool.close().await;
    }

    /// Invalid titles, kinds, fields, or artist links do not create releases.
    #[tokio::test]
    async fn rejects_invalid_input_and_missing_artists() {
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
            json!({"title": " ", "kind": "album"}),
            json!({"title": "a\0b", "kind": "album"}),
            json!({"title": "я".repeat(256), "kind": "album"}),
            json!({"title": "Valid", "kind": "unknown"}),
            json!({"title": "Valid"}),
            json!({"title": "Valid", "kind": "album", "extra": 1}),
            json!({"title": "Valid", "kind": "ep", "artist_ids": [0]}),
            json!({"title": "Valid", "kind": "ep", "artist_ids": [id, id]}),
            json!({"title": "Valid", "kind": "ep",
                "artist_ids": [id, id + 1]}),
        ] {
            let (status, _, error) =
                request(&app, "POST", "/api/v1/releases", Some(payload)).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(error["error"]["code"], "validation_error");
        }
        let (_, _, page) =
            request(&app, "GET", "/api/v1/releases", None).await;
        assert_eq!(page["items"], json!([]));
        pool.close().await;
    }

    /// Malformed queries and identifiers return 400, missing resources 404.
    #[tokio::test]
    async fn validates_query_and_resource_identifiers() {
        let (_directory, pool, app) = setup().await;
        for path in [
            "/api/v1/releases?limit=0",
            "/api/v1/releases?limit=101",
            "/api/v1/releases?offset=-1",
            "/api/v1/releases?extra=1",
            "/api/v1/releases/abc",
        ] {
            let (status, _, error) = request(&app, "GET", path, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error["error"]["code"], "bad_request");
        }
        let (status, _, error) =
            request(&app, "GET", "/api/v1/releases/1", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error["error"]["code"], "not_found");
        pool.close().await;
    }

    /// JSON syntax and media errors use the common contract; SQL errors hide.
    #[tokio::test]
    async fn extraction_and_storage_failures_use_json_envelopes() {
        let (_directory, pool, app) = setup().await;
        for (content, status) in
            [("text/plain", 415), ("application/json", 400)]
        {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/v1/releases")
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
            request(&app, "GET", "/api/v1/releases", None).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            error,
            json!({"error": {
                "code": "internal_error", "message": "Internal server error",
            }})
        );
    }
}
