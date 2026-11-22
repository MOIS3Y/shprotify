//! Private playlist CRUD and version-checked replacement of ordered tracks.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    auth::CurrentUser,
    db,
    playlists::{self, Playlist, PlaylistDetails, PlaylistTrack},
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
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Register account-scoped operations; every handler authenticates first.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_playlist, list_playlists))
        .routes(routes!(get_playlist, edit_playlist, delete_playlist))
        .routes(routes!(get_tracks, replace_tracks))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(pool)
}

/// Metadata for a newly created, empty private playlist.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CreatePlaylist {
    /// Display name; trimmed, 1–255 Unicode characters without controls.
    #[schema(min_length = 1, max_length = 255)]
    name: String,
    /// Text up to 2000 Unicode characters; defaults to empty.
    #[serde(default)]
    #[schema(max_length = 2000, default = "")]
    description: String,
}

/// Complete replacement metadata, guarded by the last observed version.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct EditPlaylist {
    /// Current version; a stale snapshot receives 409.
    #[schema(minimum = 1)]
    version: i64,
    /// New display name, with the same validation as creation.
    #[schema(min_length = 1, max_length = 255)]
    name: String,
    /// Replacement description; omission clears it.
    #[serde(default)]
    #[schema(max_length = 2000, default = "")]
    description: String,
}

/// Whole ordered list of recording occurrences, including repeated recordings.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ReplaceTracks {
    /// Current playlist version; includes both metadata and content changes.
    #[schema(minimum = 1)]
    version: i64,
    /// Desired order; at most 10000 positive IDs. Empty clears all tracks.
    #[schema(max_items = 10000)]
    recording_ids: Vec<i64>,
}

/// Explicit version precondition for deleting a private playlist.
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
struct DeleteVersion {
    /// Last observed positive playlist version.
    #[param(minimum = 1)]
    version: i64,
}

/// A bounded collection of playlist metadata belonging to the current user.
#[derive(Serialize, ToSchema)]
struct PlaylistPage {
    /// Playlists in ascending ID order.
    items: Vec<Playlist>,
    /// Maximum number returned.
    limit: i64,
    /// Number skipped.
    offset: i64,
}

/// Ordered track occurrences and the exact version of the read snapshot.
#[derive(Serialize, ToSchema)]
struct PlaylistTrackPage {
    /// Discard collected pages if their versions differ; reload before
    /// editing.
    version: i64,
    /// Recording occurrences, sorted by one-based position.
    items: Vec<PlaylistTrack>,
    /// Maximum occurrences returned.
    limit: i64,
    /// Number skipped.
    offset: i64,
}

/// Create an empty private playlist; names may repeat and do not imply
/// favorites.
#[utoipa::path(post, path = "/me/playlists", tag = "playlists",
    operation_id = "create_playlist", request_body = CreatePlaylist,
    security(("personal_key" = [])), responses(
        (status = 201, body = Playlist, description = "Playlist created",
            headers(("Location" = String, description = "Playlist URL"))),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn create_playlist(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    OriginalUri(uri): OriginalUri,
    input: Result<Json<CreatePlaylist>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = input?;
    let details = PlaylistDetails::new(&input.name, &input.description)
        .map_err(ApiError::Validation)?;
    let playlist = db::playlists::create(&pool, user.id, details).await?;
    Ok(private((
        StatusCode::CREATED,
        [(header::LOCATION, format!("{}/{}", uri.path(), playlist.id))],
        Json(playlist),
    )))
}

/// List only the authenticated account's playlist metadata.
#[utoipa::path(get, path = "/me/playlists", tag = "playlists",
    operation_id = "list_playlists", params(Pagination),
    security(("personal_key" = [])), responses(
        (status = 200, body = PlaylistPage, description = "Private playlists"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn list_playlists(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let page = pagination(query)?;
    let items =
        db::playlists::list(&pool, user.id, page.limit, page.offset).await?;
    Ok(private(Json(PlaylistPage {
        items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// Read owned metadata; other users' playlists also receive 404.
#[utoipa::path(get, path = "/me/playlists/{id}", tag = "playlists",
    operation_id = "get_playlist",
    params(("id" = i64, Path, minimum = 1, description = "Playlist ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = Playlist, description = "Owned playlist"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_playlist(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let playlist = db::playlists::find(&pool, user.id, playlist_id(id)?)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(private(Json(playlist)))
}

/// Replace name/description using the current version; preserve content order.
#[utoipa::path(put, path = "/me/playlists/{id}", tag = "playlists",
    operation_id = "edit_playlist", request_body = EditPlaylist,
    params(("id" = i64, Path, minimum = 1, description = "Playlist ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = Playlist, description = "Updated metadata"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn edit_playlist(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
    input: Result<Json<EditPlaylist>, JsonRejection>,
) -> Result<Response, ApiError> {
    let id = playlist_id(id)?;
    let Json(input) = input?;
    version(input.version)?;
    let details = PlaylistDetails::new(&input.name, &input.description)
        .map_err(ApiError::Validation)?;
    let playlist =
        db::playlists::edit(&pool, user.id, id, input.version, details)
            .await
            .map_err(playlist_error)?;
    Ok(private(Json(playlist)))
}

/// Delete metadata and occurrences only, requiring the current version query.
#[utoipa::path(delete, path = "/me/playlists/{id}", tag = "playlists",
    operation_id = "delete_playlist", params(DeleteVersion,
        ("id" = i64, Path, minimum = 1, description = "Playlist ID")),
    security(("personal_key" = [])), responses(
        (status = 204, description = "Playlist and its occurrences deleted"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn delete_playlist(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<DeleteVersion>, QueryRejection>,
) -> Result<Response, ApiError> {
    let id = playlist_id(id)?;
    let Query(query) = query.map_err(|_| {
        ApiError::BadRequest("A positive version query parameter is required")
    })?;
    if query.version <= 0 {
        return Err(ApiError::BadRequest("version must be positive"));
    }
    db::playlists::delete(&pool, user.id, id, query.version)
        .await
        .map_err(playlist_error)?;
    Ok(private(StatusCode::NO_CONTENT))
}

/// Page ordered recordings with the matching version; repeated IDs stay
/// distinct.
#[utoipa::path(get, path = "/me/playlists/{id}/tracks", tag = "playlists",
    operation_id = "get_playlist_tracks", params(Pagination,
        ("id" = i64, Path, minimum = 1, description = "Playlist ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = PlaylistTrackPage, description = "Track page"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_tracks(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let id = playlist_id(id)?;
    let page = pagination(query)?;
    let snapshot =
        db::playlists::tracks(&pool, user.id, id, page.limit, page.offset)
            .await
            .map_err(playlist_error)?;
    Ok(private(Json(PlaylistTrackPage {
        version: snapshot.version,
        items: snapshot.items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// Atomically replace order/contents; stale versions cannot lose newer edits.
/// Use a new ordered array to append, remove, repeat, reorder or clear tracks.
#[utoipa::path(put, path = "/me/playlists/{id}/tracks", tag = "playlists",
    operation_id = "replace_playlist_tracks", request_body = ReplaceTracks,
    params(("id" = i64, Path, minimum = 1, description = "Playlist ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = Playlist, description = "Updated playlist"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn replace_tracks(
    CurrentUser(user): CurrentUser,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
    input: Result<Json<ReplaceTracks>, JsonRejection>,
) -> Result<Response, ApiError> {
    let id = playlist_id(id)?;
    let Json(input) = input?;
    version(input.version)?;
    playlists::validate_tracks(&input.recording_ids)
        .map_err(ApiError::Validation)?;
    let playlist = db::playlists::replace_tracks(
        &pool,
        user.id,
        id,
        input.version,
        &input.recording_ids,
    )
    .await
    .map_err(playlist_error)?;
    Ok(private(Json(playlist)))
}

/// Validate body versions separately from malformed query preconditions.
fn version(version: i64) -> Result<(), ApiError> {
    if version <= 0 {
        return Err(ApiError::Validation("version must be positive"));
    }
    Ok(())
}

/// Validate identifiers before any database lookup.
fn playlist_id(id: Result<Path<i64>, PathRejection>) -> Result<i64, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid playlist ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("Playlist ID must be positive"));
    }
    Ok(id)
}

/// Apply the same bounded pagination and unknown-field policy to both lists.
fn pagination(
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Pagination, ApiError> {
    let Query(page) =
        query.map_err(|_| ApiError::BadRequest("Invalid pagination"))?;
    page.validate()?;
    Ok(page)
}

/// Translate expected ownership/version/reference failures into safe API
/// errors.
fn playlist_error(error: db::playlists::PlaylistError) -> ApiError {
    match error {
        db::playlists::PlaylistError::NotFound => ApiError::NotFound,
        db::playlists::PlaylistError::StaleVersion => ApiError::Conflict(
            "Playlist version changed; reload before retrying",
        ),
        db::playlists::PlaylistError::MissingRecording => {
            ApiError::Validation("One or more recordings do not exist")
        }
        db::playlists::PlaylistError::Storage(error) => error.into(),
    }
}

/// Personal playlist responses must not enter browser or shared caches.
fn private(value: impl IntoResponse) -> Response {
    let mut response = value.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response
}

#[cfg(test)]
mod tests {
    //! Private playlist HTTP contracts with real credentials and migrated
    //! SQLite.

    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{HeaderMap, Request},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Send JSON through the full router without any authorization bypass.
    async fn request(
        app: &Router,
        method: &str,
        path: &str,
        token: Option<&str>,
        value: Option<Value>,
    ) -> (u16, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match value {
            Some(value) => {
                builder =
                    builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
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

    /// Independent owners, a real member session and a shared catalog fixture.
    struct Fixture {
        /// Keep the SQLite file alive until completion.
        _directory: tempfile::TempDir,
        /// Catalog, playlists and account storage.
        pool: SqlitePool,
        /// Complete API with optional documentation enabled.
        app: Router,
        /// Ordinary member personal key.
        alice: String,
        /// Member password session for the same identity.
        session: String,
        /// Administrator who must not see another account's playlists.
        bob: String,
    }

    impl Fixture {
        /// Provision local account data without executing expensive login
        /// hashes.
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
            db::users::set_role(&pool, "bob", crate::auth::Role::Admin)
                .await
                .unwrap();
            db::sessions::set_password(&pool, "alice", "fixture-hash")
                .await
                .unwrap();
            let credential = db::sessions::credential(&pool, "alice")
                .await
                .unwrap()
                .unwrap();
            let session = crate::auth::session_token().unwrap();
            db::sessions::create(
                &pool,
                &credential,
                &crate::auth::digest(&session),
                3600,
            )
            .await
            .unwrap()
            .unwrap();
            sqlx::query(
                "INSERT INTO recordings (id,title) VALUES (1,'One'),(2,'Two')",
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
                session,
                bob,
            }
        }

        /// Send a request as the playlist owner.
        async fn alice(
            &self,
            method: &str,
            path: &str,
            value: Option<Value>,
        ) -> (u16, HeaderMap, Value) {
            request(&self.app, method, path, Some(&self.alice), value).await
        }
    }

    /// Duplicates, reorders, edits and deletion stay separate from favorites.
    #[tokio::test]
    async fn implements_private_playlist_lifecycle_and_ordered_occurrences() {
        let fixture = Fixture::new().await;
        let collection = "/api/v1/me/playlists";
        fixture
            .alice(
                "PUT",
                "/api/v1/me/recordings/1/reaction",
                Some(json!({"reaction":"like"})),
            )
            .await;
        fixture
            .alice(
                "POST",
                "/api/v1/me/history",
                Some(json!({
                    "event_id":"listen", "recording_id":1, "started_at":0,
                    "listened_ms":100
                })),
            )
            .await;
        let created = fixture
            .alice(
                "POST",
                collection,
                Some(json!({
                    "name":"  Избранное  ","description":" hello\nworld "
                })),
            )
            .await;
        assert_eq!(created.0, 201);
        assert_eq!(created.1[header::LOCATION], "/api/v1/me/playlists/1");
        assert_eq!(created.1[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(created.2["name"], "Избранное");
        assert_eq!(created.2["description"], "hello\nworld");
        assert_eq!(created.2["version"], 1);
        assert!(created.2.get("user_id").is_none());
        let url = "/api/v1/me/playlists/1";
        let tracks = "/api/v1/me/playlists/1/tracks";
        assert_eq!(
            fixture.alice("GET", tracks, None).await.2["items"],
            json!([])
        );
        let updated = fixture
            .alice(
                "PUT",
                tracks,
                Some(json!({
                    "version":1,"recording_ids":[1,2,1]
                })),
            )
            .await;
        assert_eq!(updated.0, 200);
        assert_eq!(updated.2["version"], 2);
        let same = fixture
            .alice(
                "PUT",
                tracks,
                Some(json!({
                    "version":2,"recording_ids":[1,2,1]
                })),
            )
            .await;
        assert_eq!(same.2, updated.2);
        let page = fixture
            .alice("GET", &format!("{tracks}?limit=2&offset=1"), None)
            .await;
        assert_eq!(page.1[header::CACHE_CONTROL], "private, no-store");
        assert_eq!(
            page.2,
            json!({"version":2,"limit":2,"offset":1,"items":[
                {"position":2,"recording_id":2,"title":"Two"},
                {"position":3,"recording_id":1,"title":"One"}
            ]})
        );
        assert_eq!(
            fixture
                .alice(
                    "PUT",
                    tracks,
                    Some(json!({
                        "version":1,"recording_ids":[2]
                    }))
                )
                .await
                .0,
            409
        );
        assert_eq!(
            fixture
                .alice(
                    "PUT",
                    tracks,
                    Some(json!({
                        "version":2,"recording_ids":[999]
                    }))
                )
                .await
                .0,
            422
        );
        assert_eq!(fixture.alice("GET", url, None).await.2["version"], 2);
        let reordered = fixture
            .alice(
                "PUT",
                tracks,
                Some(json!({
                    "version":2,"recording_ids":[2,1]
                })),
            )
            .await;
        assert_eq!(reordered.2["version"], 3);
        let renamed = fixture
            .alice(
                "PUT",
                url,
                Some(json!({
                    "version":3,"name":"Road"
                })),
            )
            .await;
        assert_eq!(renamed.2["version"], 4);
        assert_eq!(renamed.2["description"], "");
        assert_eq!(
            fixture
                .alice("DELETE", &format!("{url}?version=3"), None)
                .await
                .0,
            409
        );
        assert_eq!(
            fixture.alice("GET", tracks, None).await.2["items"],
            json!([
                {"position":1,"recording_id":2,"title":"Two"},
                {"position":2,"recording_id":1,"title":"One"}
            ])
        );
        assert_eq!(
            fixture
                .alice("POST", collection, Some(json!({"name":"Road"})))
                .await
                .0,
            201
        );
        assert_eq!(
            fixture
                .alice("GET", &format!("{collection}?limit=1&offset=1"), None)
                .await
                .2["items"][0]["id"],
            2
        );
        let session_read =
            request(&fixture.app, "GET", url, Some(&fixture.session), None)
                .await;
        assert_eq!(session_read.2, renamed.2);
        assert_eq!(
            fixture
                .alice(
                    "PUT",
                    tracks,
                    Some(json!({
                        "version":4,"recording_ids":[]
                    }))
                )
                .await
                .2["version"],
            5
        );
        let deleted = fixture
            .alice("DELETE", &format!("{url}?version=5"), None)
            .await;
        assert_eq!(deleted.0, 204);
        assert!(deleted.2.is_null());
        assert_eq!(fixture.alice("GET", url, None).await.0, 404);
        let favorites =
            fixture.alice("GET", "/api/v1/me/favorites", None).await.2;
        assert_eq!(favorites["items"][0]["recording_id"], 1);
        assert_eq!(
            fixture.alice("GET", "/api/v1/me/history", None).await.2["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let recordings: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM recordings")
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
        assert_eq!(recordings, 2);
        fixture.pool.close().await;
    }

    /// Every route authenticates before parsing; admin does not bypass
    /// ownership.
    #[tokio::test]
    async fn isolates_accounts_and_checks_authentication_before_inputs() {
        let fixture = Fixture::new().await;
        let collection = "/api/v1/me/playlists";
        fixture
            .alice("POST", collection, Some(json!({"name":"Private"})))
            .await;
        for (method, path, body) in [
            ("POST", collection, Some(json!({}))),
            ("GET", "/api/v1/me/playlists?limit=invalid", None),
            ("GET", "/api/v1/me/playlists/x", None),
            ("HEAD", "/api/v1/me/playlists/x/tracks", None),
            ("PUT", "/api/v1/me/playlists/x", Some(json!({}))),
            ("DELETE", "/api/v1/me/playlists/x?version=bad", None),
            ("GET", "/api/v1/me/playlists/x/tracks", None),
            ("PUT", "/api/v1/me/playlists/x/tracks", Some(json!({}))),
        ] {
            let (status, headers, _) =
                request(&fixture.app, method, path, None, body).await;
            assert_eq!(status, 401);
            assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
        }
        for id in ["1", "999"] {
            let url = format!("{collection}/{id}");
            for (method, path, body) in [
                ("GET", url.clone(), None),
                (
                    "PUT",
                    url.clone(),
                    Some(json!({"version":99,"name":"Take"})),
                ),
                ("DELETE", format!("{url}?version=99"), None),
                ("GET", format!("{url}/tracks"), None),
                (
                    "PUT",
                    format!("{url}/tracks"),
                    Some(json!({"version":99,"recording_ids":[1]})),
                ),
            ] {
                assert_eq!(
                    request(
                        &fixture.app,
                        method,
                        &path,
                        Some(&fixture.bob),
                        body
                    )
                    .await
                    .0,
                    404
                );
            }
        }
        assert_eq!(
            request(&fixture.app, "GET", collection, Some(&fixture.bob), None)
                .await
                .2["items"],
            json!([])
        );
        db::users::revoke(&fixture.pool, "alice").await.unwrap();
        assert_eq!(fixture.alice("GET", collection, None).await.0, 401);
        assert_eq!(
            request(
                &fixture.app,
                "GET",
                collection,
                Some(&fixture.session),
                None
            )
            .await
            .2["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        fixture.pool.close().await;
    }

    /// Reject invalid fields, unknown IDs, excessive arrays and stale
    /// versions.
    #[tokio::test]
    async fn validates_metadata_order_and_version_preconditions() {
        let fixture = Fixture::new().await;
        let collection = "/api/v1/me/playlists";
        for body in [
            json!({}),
            json!({"name":"  "}),
            json!({"name":"bad\nname"}),
            json!({"name":"a".repeat(256)}),
            json!({"name":"x","description":"a".repeat(2001)}),
            json!({"name":"x","description":"bad\0text"}),
            json!({"name":"x","user_id":2}),
            json!({"name":"x","public":true}),
        ] {
            assert_eq!(
                fixture.alice("POST", collection, Some(body)).await.0,
                422
            );
        }
        assert_eq!(fixture.alice("POST", collection, None).await.0, 415);
        assert_eq!(
            fixture
                .alice(
                    "POST",
                    collection,
                    Some(json!({
                        "name":"x","description":"a".repeat(300_000)
                    }))
                )
                .await
                .0,
            413
        );
        fixture
            .alice("POST", collection, Some(json!({"name":"Test"})))
            .await;
        let url = "/api/v1/me/playlists/1";
        let tracks = "/api/v1/me/playlists/1/tracks";
        for body in [
            json!({}),
            json!({"version":0,"name":"x"}),
            json!({"version":1,"name":"x","user_id":2}),
        ] {
            assert_eq!(fixture.alice("PUT", url, Some(body)).await.0, 422);
        }
        for body in [
            json!({}),
            json!({"version":0,"recording_ids":[]}),
            json!({"version":1,"recording_ids":[0]}),
            json!({"version":1,"recording_ids":[-1]}),
            json!({"version":1,"recording_ids":[999]}),
            json!({"version":1,"recording_ids":[null]}),
        ] {
            assert_eq!(fixture.alice("PUT", tracks, Some(body)).await.0, 422);
        }
        assert_eq!(fixture.alice("PUT", tracks, Some(json!({
            "version":1,"recording_ids":vec![1; playlists::MAX_TRACKS + 1]
        }))).await.0, 422);
        for base in [collection, tracks] {
            for query in ["limit=0", "limit=101", "offset=-1", "user_id=2"] {
                assert_eq!(
                    fixture
                        .alice("GET", &format!("{base}?{query}"), None)
                        .await
                        .0,
                    400
                );
            }
        }
        for query in ["", "?version=0", "?version=bad", "?version=1&user_id=2"]
        {
            assert_eq!(
                fixture
                    .alice("DELETE", &format!("{url}{query}"), None)
                    .await
                    .0,
                400
            );
        }
        assert_eq!(fixture.alice("GET", url, None).await.2["version"], 1);
        assert_eq!(
            fixture.alice("GET", tracks, None).await.2["items"],
            json!([])
        );
        fixture.pool.close().await;
    }

    /// OpenAPI describes private operations, ordered snapshots and edit
    /// versions.
    #[tokio::test]
    async fn documents_all_playlist_operations_and_bounded_inputs() {
        let fixture = Fixture::new().await;
        let schema = request(&fixture.app, "GET", "/openapi.json", None, None)
            .await
            .2;
        let mut count = 0;
        for (path, methods) in schema["paths"].as_object().unwrap() {
            if !path.starts_with("/api/v1/me/playlists") {
                continue;
            }
            for method in ["get", "post", "put", "delete"] {
                let operation = &methods[method];
                if operation.is_null() {
                    continue;
                }
                count += 1;
                assert!(operation["security"][0]["personal_key"].is_array());
                assert!(operation["responses"]["401"].is_object());
            }
        }
        assert_eq!(count, 7);
        let models = &schema["components"]["schemas"];
        assert_eq!(
            models["ReplaceTracks"]["properties"]["recording_ids"]["maxItems"],
            playlists::MAX_TRACKS
        );
        assert!(models["Playlist"]["properties"].get("user_id").is_none());
        let required = models["EditPlaylist"]["required"].as_array().unwrap();
        assert!(required.contains(&json!("version")));
        assert!(required.contains(&json!("name")));
        fixture.pool.close().await;
    }
}
