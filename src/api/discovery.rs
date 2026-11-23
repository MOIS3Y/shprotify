//! Private discovery profiles, persisted suggestions and acquisition intent.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    auth::CurrentUser,
    db::{
        self,
        discovery::{DiscoveryError, DiscoveryJob, Feed},
    },
    discovery::{FeedRequest, Feedback, Preferences, Profile, Recommendation},
    jobs::discovery::DiscoveryService,
};
use axum::{
    Json,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Storage and service are cloned without spawning any background tasks.
#[derive(Clone)]
struct DiscoveryState {
    /// Same catalog pool used by authentication and personal data access.
    pool: SqlitePool,
    /// Application-owned worker lifecycle.
    service: DiscoveryService,
}
impl axum::extract::FromRef<DiscoveryState> for SqlitePool {
    /// Authenticate through the same database as discovery ownership checks.
    fn from_ref(state: &DiscoveryState) -> Self {
        state.pool.clone()
    }
}

/// Mount personal routes with authentication before validation and mutations.
pub(super) fn router(
    pool: SqlitePool,
    service: DiscoveryService,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(get_profile))
        .routes(routes!(set_preferences))
        .routes(routes!(create_feed, list_feeds))
        .routes(routes!(get_feed))
        .routes(routes!(get_items))
        .routes(routes!(refresh_feed))
        .routes(routes!(get_candidate))
        .routes(routes!(set_feedback))
        .routes(routes!(get_wishlist))
        .routes(routes!(get_decisions))
        .routes(routes!(list_jobs))
        .routes(routes!(get_job))
        .routes(routes!(export_playlist))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(DiscoveryState { pool, service })
}

/// Query filters apply to the complete collection before pagination.
#[derive(Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
#[into_params(parameter_in = Query)]
struct ItemQuery {
    /// Maximum number of results, default 50, range 1–100.
    #[param(minimum = 1, maximum = 100, default = 50)]
    limit: i64,
    /// Number of visible candidates to skip.
    #[param(minimum = 0, default = 0)]
    offset: i64,
    /// Exclude candidates matched to any registered audio file.
    missing_only: bool,
    /// Include hidden/disliked items for reviewing or undoing decisions.
    include_hidden: bool,
}
impl Default for ItemQuery {
    /// First 50 visible suggestions, including local and external candidates.
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            missing_only: false,
            include_hidden: false,
        }
    }
}

/// Source-content version and live personal projection of a bounded page.
#[derive(Serialize, ToSchema)]
struct DiscoveryPage {
    /// Feed generation version; decisions/local catalog can change separately.
    version: i64,
    /// Visible suggestions after ranking and filters.
    items: Vec<Recommendation>,
    /// Number satisfying the current live filters.
    total: usize,
    /// Page size.
    limit: i64,
    /// Number skipped.
    offset: i64,
}

/// Complete request for copying matched recordings into an ordinary playlist.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct ExportPlaylist {
    /// Last observed positive feed generation version.
    version: i64,
    /// Display name of the new private playlist.
    name: String,
}

/// New playlist plus counts of local and unexportable external candidates.
#[derive(Serialize, ToSchema)]
struct PlaylistExport {
    /// Fresh private ordinary playlist; future feed refreshes never change it.
    playlist: crate::playlists::Playlist,
    /// Number of matched recording occurrences copied in source order.
    included: usize,
    /// Unmatched, release, hidden or disliked candidates not copied.
    skipped: usize,
}

/// Personal job or feed collection page.
#[derive(Serialize, ToSchema)]
struct FeedPage {
    /// Owned feed definitions, newest first.
    items: Vec<Feed>,
    /// Page size.
    limit: i64,
    /// Number skipped.
    offset: i64,
}

/// Bounded refresh history scoped to this account.
#[derive(Serialize, ToSchema)]
struct JobPage {
    /// Newest job first, including failed and interrupted refreshes.
    items: Vec<DiscoveryJob>,
    /// Page size.
    limit: i64,
    /// Number skipped.
    offset: i64,
}

/// Wishlist page retains wanted candidates after source collection refreshes.
#[derive(Serialize, ToSchema)]
struct WishlistPage {
    /// Current candidate metadata/local matches without suppression filters.
    items: Vec<Recommendation>,
    /// Page size.
    limit: i64,
    /// Number skipped.
    offset: i64,
}

/// Keep expected domain failures stable and storage diagnostics private.
impl From<DiscoveryError> for ApiError {
    /// Ownership checks conceal another account's feed, job or candidate.
    fn from(error: DiscoveryError) -> Self {
        match error {
            DiscoveryError::NotFound => Self::NotFound,
            DiscoveryError::Conflict => Self::Conflict(
                "Discovery changed, is busy, or is cooling down; reload later",
            ),
            DiscoveryError::Invalid(message) => Self::Validation(message),
            DiscoveryError::Storage(error) => error.into(),
            DiscoveryError::Json(error) => Self::Internal(error.into()),
        }
    }
}

/// Parse IDs only after the handler has authenticated the request.
fn identifier(
    path: Result<Path<i64>, PathRejection>,
) -> Result<i64, ApiError> {
    let Path(id) =
        path.map_err(|_| ApiError::BadRequest("Invalid discovery ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("ID must be positive"));
    }
    Ok(id)
}

/// Validate bounded collection query parameters with the existing contract.
fn page(
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Pagination, ApiError> {
    let Query(page) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    page.validate()?;
    Ok(page)
}

/// Prevent taste profiles, queries and private feedback from entering caches.
fn private(value: impl IntoResponse) -> Response {
    let mut response = value.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

/// Accepted refreshes are queried through the returned account-scoped URL.
fn accepted(job: DiscoveryJob) -> Response {
    private((
        StatusCode::ACCEPTED,
        [(
            header::LOCATION,
            format!("/api/v1/me/discovery/jobs/{}", job.id),
        )],
        Json(job),
    ))
}

/// Read explicit and inferred preferences without contacting providers.
#[utoipa::path(get, path = "/me/discovery/profile", tag = "Discovery",
    operation_id = "get_profile",
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = Profile),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_profile(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
) -> Result<Response, ApiError> {
    Ok(private(Json(
        db::taste::profile(&state.pool, user.id).await?,
    )))
}

/// Replace onboarding artist follows/tags using a version precondition.
#[utoipa::path(put, path = "/me/discovery/preferences", tag = "Discovery",
    operation_id = "set_preferences",
    request_body = Preferences,
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = Preferences),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn set_preferences(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    input: Result<Json<Preferences>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = input?;
    Ok(private(Json(
        db::taste::set_preferences(&state.pool, user.id, input).await?,
    )))
}

/// Generate or refresh an owned collection; external results stay outside
/// catalog.
#[utoipa::path(post, path = "/me/discovery/feeds", tag = "Discovery",
    operation_id = "create_feed",
    request_body = FeedRequest,
    security(("personal_key" = [])), responses(
        (status = 202, description = "Personal discovery result",
            body = DiscoveryJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn create_feed(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    input: Result<Json<FeedRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = input?;
    Ok(accepted(state.service.start(user.id, input).await?))
}

/// List only this account's saved discovery collections.
#[utoipa::path(get, path = "/me/discovery/feeds", tag = "Discovery",
    operation_id = "list_feeds",
    params(Pagination),
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = FeedPage),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn list_feeds(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let page = page(query)?;

    let items =
        db::discovery::feeds(&state.pool, user.id, page.limit, page.offset)
            .await?;
    Ok(private(Json(FeedPage {
        items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// Read collection metadata, provenance, generation time and safe warnings.
#[utoipa::path(get, path = "/me/discovery/feeds/{id}", tag = "Discovery",
    operation_id = "get_feed",
    params(("id" = i64, Path, minimum = 1)),
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = Feed),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_feed(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    Ok(private(Json(
        db::discovery::feed(&state.pool, user.id, id).await?,
    )))
}

/// Read live-ranked suggestions or original source order with private filters.
#[utoipa::path(get, path = "/me/discovery/feeds/{id}/items", tag = "Discovery",
    operation_id = "get_items",
    params(("id" = i64, Path, minimum = 1),ItemQuery),
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = DiscoveryPage),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_items(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    path: Result<Path<i64>, PathRejection>,
    query: Result<Query<ItemQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;

    let Query(query) = query
        .map_err(|_| ApiError::BadRequest("Invalid discovery filters"))?;
    let paging = Pagination {
        limit: query.limit,
        offset: query.offset,
    };
    paging.validate()?;
    let (feed, items) =
        db::discovery::contents(&state.pool, user.id, id).await?;
    let profile = db::taste::profile(&state.pool, user.id).await?;
    let mut items = crate::discovery::ranking::rank(
        items,
        &profile,
        feed.request.personalized(),
        query.include_hidden,
        query.missing_only,
    );
    if matches!(feed.request, FeedRequest::Personal {})
        && !query.include_hidden
    {
        items.retain(|i| {
            i.reaction != Some(crate::library::reactions::Reaction::Like)
        });
    }
    let total = items.len();
    let items = items
        .into_iter()
        .skip(query.offset as usize)
        .take(query.limit as usize)
        .collect();
    Ok(private(Json(DiscoveryPage {
        version: feed.version,
        items,
        total,
        limit: query.limit,
        offset: query.offset,
    })))
}

/// Refresh a saved definition; old contents survive failed external requests.
#[utoipa::path(post, path = "/me/discovery/feeds/{id}/refresh",
    tag = "Discovery",
    operation_id = "refresh_feed",
    params(("id" = i64, Path, minimum = 1)),
    security(("personal_key" = [])), responses(
        (status = 202, description = "Personal discovery result",
            body = DiscoveryJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn refresh_feed(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    Ok(accepted(state.service.refresh(user.id, id).await?))
}

/// Read a previously seen candidate, including a hidden retained decision.
#[utoipa::path(get, path = "/me/discovery/candidates/{id}", tag = "Discovery",
    operation_id = "get_candidate",
    params(("id" = i64, Path, minimum = 1)),
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = Recommendation),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_candidate(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    Ok(private(Json(
        db::discovery::candidate(&state.pool, user.id, id).await?,
    )))
}

/// Like/dislike, hide or retain acquisition intent before downloading exists.
#[utoipa::path(put, path = "/me/discovery/candidates/{id}/feedback",
    tag = "Discovery",
    operation_id = "set_feedback",
    params(("id" = i64, Path, minimum = 1)),
    request_body = Feedback,
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = Feedback),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn set_feedback(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    path: Result<Path<i64>, PathRejection>,
    input: Result<Json<Feedback>, JsonRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    let Json(input) = input?;
    Ok(private(Json(
        db::discovery::feedback(&state.pool, user.id, id, input).await?,
    )))
}

/// Read acquisition intent, including external recordings and whole releases.
#[utoipa::path(get, path = "/me/discovery/wishlist", tag = "Discovery",
    operation_id = "get_wishlist",
    params(Pagination),
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = WishlistPage),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_wishlist(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let page = page(query)?;

    let items =
        db::discovery::wishlist(&state.pool, user.id, page.limit, page.offset)
            .await?;
    Ok(private(Json(WishlistPage {
        items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// List only this account's refresh history.
#[utoipa::path(get, path = "/me/discovery/jobs", tag = "Discovery",
    operation_id = "list_jobs",
    params(Pagination),
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = JobPage),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn list_jobs(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let page = page(query)?;

    let items =
        db::discovery::jobs(&state.pool, user.id, page.limit, page.offset)
            .await?;
    Ok(private(Json(JobPage {
        items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// Read one owned refresh status, including crash interruption.
#[utoipa::path(get, path = "/me/discovery/jobs/{id}", tag = "Discovery",
    operation_id = "get_discovery_job",
    params(("id" = i64, Path, minimum = 1)),
    security(("personal_key" = [])), responses(
        (status = 200, description = "Personal discovery result",
            body = DiscoveryJob),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_job(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;
    Ok(private(Json(
        db::discovery::job(&state.pool, user.id, id).await?,
    )))
}

/// Copy eligible matched recordings into a private ordinary playlist.
#[utoipa::path(post, path = "/me/discovery/feeds/{id}/playlist",
    tag = "Discovery",
    operation_id = "export_playlist",
    params(("id" = i64, Path, minimum = 1)),
    request_body = ExportPlaylist,
    security(("personal_key" = [])), responses(
        (status = 201, description = "Personal discovery result",
            body = PlaylistExport),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn export_playlist(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    path: Result<Path<i64>, PathRejection>,
    input: Result<Json<ExportPlaylist>, JsonRejection>,
) -> Result<Response, ApiError> {
    let id = identifier(path)?;

    let Json(input) = input?;
    if input.version <= 0 {
        return Err(ApiError::Validation("version must be positive"));
    }
    let details = crate::playlists::PlaylistDetails::new(&input.name, "")
        .map_err(ApiError::Validation)?;
    let (playlist, included, skipped) = db::discovery::export_playlist(
        &state.pool,
        user.id,
        id,
        input.version,
        details,
    )
    .await?;
    let location = format!("/api/v1/me/playlists/{}", playlist.id);
    Ok(private((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(PlaylistExport {
            playlist,
            included,
            skipped,
        }),
    )))
}

/// Review discovery decisions independently of current feed contents.
#[derive(Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
#[into_params(parameter_in = Query)]
struct DecisionQuery {
    /// Maximum items, default 50, range 1–100.
    limit: i64,
    /// Number skipped, nonnegative.
    offset: i64,
    /// Filter last discovery assessment, not later native Recording edits.
    reaction: Option<crate::library::reactions::Reaction>,
    /// Review hidden=true decisions or visible=false; omission includes both.
    hidden: Option<bool>,
}
impl Default for DecisionQuery {
    /// Review the first 50 decisions, including hidden candidates.
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            reaction: None,
            hidden: None,
        }
    }
}

/// Review prior likes, dislikes and hidden items after a source feed refresh.
#[utoipa::path(get, path = "/me/discovery/feedback", tag = "Discovery",
    operation_id = "list_discovery_feedback", params(DecisionQuery),
    security(("personal_key" = [])),responses(
        (status = 200, description = "Retained discovery decisions",
            body = WishlistPage),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_decisions(
    CurrentUser(user): CurrentUser,
    State(state): State<DiscoveryState>,
    query: Result<Query<DecisionQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query.map_err(|_| {
        ApiError::BadRequest("Invalid discovery feedback filters")
    })?;
    Pagination {
        limit: query.limit,
        offset: query.offset,
    }
    .validate()?;
    let items = db::discovery::decisions(
        &state.pool,
        user.id,
        db::discovery::DecisionFilter {
            reaction: query.reaction,
            hidden: query.hidden,
            wanted: None,
        },
        query.limit,
        query.offset,
    )
    .await?;
    Ok(private(Json(WishlistPage {
        items,
        limit: query.limit,
        offset: query.offset,
    })))
}

#[cfg(test)]
mod tests {
    //! Real credentials, private IDs, strict validation and offline workflows.
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{HeaderMap, Request},
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

    /// File-backed API fixture retaining the worker until explicit shutdown.
    struct Fixture {
        /// Database lifetime guard.
        _dir: tempfile::TempDir,
        /// Catalog and personal discovery storage.
        pool: SqlitePool,
        /// Real complete versioned HTTP router.
        app: Router,
        /// Owned service to drain before closing SQLite.
        service: DiscoveryService,
        /// Ordinary member discovering music.
        alice: String,
        /// Administrator with no access to Alice's discovery.
        bob: String,
    }
    impl Fixture {
        /// Seed recordings and issue real keys; no authentication bypass.
        async fn new() -> Self {
            let (dir, pool) = db::discovery::tests::setup().await;
            let alice = crate::auth::token().unwrap();
            let bob = crate::auth::token().unwrap();
            for (user, key) in [(1, &alice), (2, &bob)] {
                sqlx::query(
                    "INSERT INTO user_keys (user_id,digest) VALUES (?,?)",
                )
                .bind(user)
                .bind(crate::auth::digest(key))
                .execute(&pool)
                .await
                .unwrap();
            }
            db::users::set_role(&pool, "bob", crate::auth::Role::Admin)
                .await
                .unwrap();
            let service = DiscoveryService::new(
                pool.clone(),
                crate::config::DiscoveryConfig::default(),
            )
            .unwrap();
            let scans = crate::jobs::ScanService::new(pool.clone(), vec![]);
            let metadata = crate::jobs::metadata::MetadataService::new(
                pool.clone(),
                scans.roots(),
                crate::config::MetadataConfig::default(),
            )
            .unwrap();
            let app = crate::api::router_with_services(
                &crate::config::AuthConfig::default(),
                &crate::config::DocsConfig { enabled: true },
                &pool,
                crate::api::Services {
                    scans,
                    metadata,
                    discovery: service.clone(),
                    downloads: crate::jobs::downloads::DownloadService::new(
                        pool.clone(),
                        crate::config::DownloadsConfig::default(),
                        None,
                    ),
                    automation: crate::config::AutomationConfig::default(),
                    clients: crate::config::ClientSettings::default(),
                    removals: crate::jobs::removals::RemovalService::new(
                        pool.clone(),
                        None,
                    ),
                },
            );
            Self {
                _dir: dir,
                pool,
                app,
                service,
                alice,
                bob,
            }
        }
    }

    /// The full local workflow needs no external credentials or audio files.
    #[tokio::test]
    async fn generates_offline_personal_feed_and_keeps_feedback_private() {
        let f = Fixture::new().await;
        let root = "/api/v1/me/discovery";
        let pref = request(
            &f.app,
            "PUT",
            &format!("{root}/preferences"),
            Some(&f.alice),
            Some(json!({"version":0,"artist_ids":[1],
                "tags":["русский рок"]})),
        )
        .await;
        assert_eq!(pref.0, 200);
        let (status, headers, job) = request(
            &f.app,
            "POST",
            &format!("{root}/feeds"),
            Some(&f.alice),
            Some(json!({"kind":"personal"})),
        )
        .await;
        assert_eq!(status, 202);
        assert_eq!(headers[header::CACHE_CONTROL], "private, no-store");
        let job_url = headers[header::LOCATION].to_str().unwrap();
        f.service.close().await;
        let completed =
            request(&f.app, "GET", job_url, Some(&f.alice), None).await;
        assert_eq!(completed.2["status"], "completed");
        assert_eq!(
            request(&f.app, "GET", job_url, Some(&f.bob), None).await.0,
            404
        );
        let feed = format!("{root}/feeds/{}", job["feed_id"]);
        let page = request(
            &f.app,
            "GET",
            &format!("{feed}/items?limit=2"),
            Some(&f.alice),
            None,
        )
        .await;
        assert_eq!(page.0, 200);
        assert_eq!(page.2["total"], 3);
        assert_eq!(page.2["items"].as_array().unwrap().len(), 2);
        let item = &page.2["items"][0];
        assert_eq!(item["candidate"]["artists"][0]["name"], "Кино");
        assert!(item["reasons"].as_array().unwrap().len() >= 2);
        let candidate = format!("{root}/candidates/{}", item["id"]);
        let feedback = format!("{candidate}/feedback");
        assert_eq!(
            request(&f.app, "GET", &candidate, Some(&f.bob), None)
                .await
                .0,
            404
        );
        let liked = request(
            &f.app,
            "PUT",
            &feedback,
            Some(&f.alice),
            Some(json!({"version":0,"reaction":"like","hidden":false,
                "wanted":true})),
        )
        .await;
        assert_eq!(liked.0, 200);
        assert_eq!(liked.2["version"], 1);
        assert_eq!(
            request(
                &f.app,
                "GET",
                "/api/v1/me/favorites",
                Some(&f.alice),
                None
            )
            .await
            .2["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            request(
                &f.app,
                "GET",
                &format!("{root}/wishlist"),
                Some(&f.alice),
                None
            )
            .await
            .2["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            request(
                &f.app,
                "GET",
                &format!("{root}/wishlist"),
                Some(&f.bob),
                None
            )
            .await
            .2["items"],
            json!([])
        );
        let hidden = request(
            &f.app,
            "PUT",
            &feedback,
            Some(&f.alice),
            Some(json!({"version":1,"reaction":"like","hidden":true,
                "wanted":true})),
        )
        .await;
        assert_eq!(hidden.0, 200);
        let visible = request(
            &f.app,
            "GET",
            &format!("{feed}/items"),
            Some(&f.alice),
            None,
        )
        .await
        .2;
        assert_eq!(visible["total"], 2);
        let reviewed = request(
            &f.app,
            "GET",
            &format!("{feed}/items?include_hidden=true"),
            Some(&f.alice),
            None,
        )
        .await
        .2;
        assert_eq!(reviewed["total"], 3);
        let decisions = request(
            &f.app,
            "GET",
            &format!("{root}/feedback?hidden=true&reaction=like"),
            Some(&f.alice),
            None,
        )
        .await;
        assert_eq!(decisions.2["items"].as_array().unwrap().len(), 1);
        let exported = request(
            &f.app,
            "POST",
            &format!("{feed}/playlist"),
            Some(&f.alice),
            Some(json!({"version":1,"name":"Discovered"})),
        )
        .await;
        assert_eq!(exported.0, 201);
        assert_eq!(exported.2["included"], 2);
        assert_eq!(exported.2["skipped"], 1);
        assert_eq!(exported.2["playlist"]["is_public"], false);
        assert_eq!(
            request(&f.app, "GET", &feed, Some(&f.bob), None).await.0,
            404
        );
        assert_eq!(
            request(
                &f.app,
                "GET",
                &format!("{root}/jobs"),
                Some(&f.bob),
                None
            )
            .await
            .2["items"],
            json!([])
        );
        assert_eq!(
            request(
                &f.app,
                "GET",
                &format!("{root}/feeds"),
                Some(&f.bob),
                None
            )
            .await
            .2["items"],
            json!([])
        );
        f.pool.close().await;
    }

    /// Authentication wins over malformed IDs/bodies; filters are bounded.
    #[tokio::test]
    async fn validates_requests_and_documents_personal_operations() {
        let f = Fixture::new().await;
        let root = "/api/v1/me/discovery";
        for (method, suffix) in [
            ("GET", "/profile"),
            ("PUT", "/preferences"),
            ("POST", "/feeds"),
            ("GET", "/feeds"),
            ("GET", "/feeds/bad"),
            ("GET", "/feeds/1/items"),
            ("POST", "/feeds/1/refresh"),
            ("GET", "/candidates/1"),
            ("PUT", "/candidates/1/feedback"),
            ("GET", "/wishlist"),
            ("GET", "/feedback"),
            ("GET", "/jobs"),
            ("GET", "/jobs/1"),
            ("POST", "/feeds/1/playlist"),
        ] {
            let result = request(
                &f.app,
                method,
                &format!("{root}{suffix}"),
                None,
                Some(json!({"bad":true})),
            )
            .await;
            assert_eq!(result.0, 401, "{method} {suffix}");
            assert_eq!(result.1[header::WWW_AUTHENTICATE], "Bearer");
        }
        for body in [
            json!({"kind":"chart"}),
            json!({"kind":"chart","extra":true}),
            json!({"kind":"similar_track","artist":"","track":"x"}),
            json!({"kind":"playlist","playlist_id":"../secret"}),
        ] {
            assert_eq!(
                request(
                    &f.app,
                    "POST",
                    &format!("{root}/feeds"),
                    Some(&f.alice),
                    Some(body)
                )
                .await
                .0,
                422
            );
        }
        for suffix in [
            "/feeds?limit=0",
            "/jobs?offset=-1",
            "/wishlist?user_id=2",
            "/feeds/bad",
        ] {
            assert_eq!(
                request(
                    &f.app,
                    "GET",
                    &format!("{root}{suffix}"),
                    Some(&f.alice),
                    None
                )
                .await
                .0,
                400
            );
        }
        let schema =
            request(&f.app, "GET", "/openapi.json", None, None).await.2;
        let mut count = 0;
        for (path, methods) in schema["paths"].as_object().unwrap() {
            if path.starts_with(root) {
                for method in ["get", "post", "put"] {
                    if !methods[method].is_null() {
                        count += 1;
                        assert_eq!(
                            methods[method]["security"],
                            json!([{"personal_key":[]}])
                        );
                    }
                }
            }
        }
        assert_eq!(count, 16);
        f.service.close().await;
        f.pool.close().await;
    }
}
