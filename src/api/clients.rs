//! Client capabilities, catalog access policy and explicit browser origins.

use axum::{
    Json, Router,
    extract::{FromRequestParts, Request, State},
    http::{Method, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::error::{ApiError, ErrorResponse};
use crate::{auth::CurrentUser, config::ClientSettings, db};

/// Immutable subset of server policy useful before a client signs in.
#[derive(Clone, Serialize, ToSchema)]
struct Capabilities {
    /// Public API namespace, independent of package version.
    api_version: &'static str,
    /// Server package version for diagnostics.
    server_version: &'static str,
    /// Whether catalog reads work without authentication.
    public_catalog: bool,
    /// Default and maximum catalog page sizes.
    pagination: PageLimits,
    /// Maximum distinct IDs in either recording or reaction batch reads.
    batch_limit: usize,
    /// Maximum persisted queue or playlist length.
    collection_limit: usize,
    /// Lifetime of a newly issued media URL; parent expiry may shorten it.
    playback_ticket_ttl_seconds: u32,
    /// Whether the experimental acquisition provider is enabled.
    downloads_enabled: bool,
    /// Whether interval automation is enabled.
    automation_enabled: bool,
}

/// Advertised bounds match the shared catalog pagination contract.
#[derive(Clone, Serialize, ToSchema)]
struct PageLimits {
    /// Page size when omitted.
    default: i64,
    /// Largest accepted page size.
    maximum: i64,
}

/// Register public policy discovery separately from protected catalog reads.
pub(super) fn router(
    settings: &ClientSettings,
    downloads_enabled: bool,
    automation_enabled: bool,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(capabilities))
        .with_state(Capabilities {
            api_version: "v1",
            server_version: env!("CARGO_PKG_VERSION"),
            public_catalog: settings.public_catalog,
            pagination: PageLimits {
                default: 50,
                maximum: 100,
            },
            collection_limit: crate::playlists::MAX_TRACKS,
            batch_limit: 100,
            playback_ticket_ttl_seconds: settings.ticket_ttl_seconds,
            downloads_enabled,
            automation_enabled,
        })
}

/// Discover safe server policy without exposing paths or provider secrets.
#[utoipa::path(get, path = "/capabilities", tag = "System",
    responses((status = 200, body = Capabilities,
        description = "Client capabilities and startup policy")))]
async fn capabilities(
    State(policy): State<Capabilities>,
) -> Json<Capabilities> {
    Json(policy)
}

/// Mount the library overview with other reads governed by catalog policy.
pub(super) fn catalog(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new().routes(routes!(stats)).with_state(pool)
}

/// Count catalog identities separately from recordings with registered audio.
#[utoipa::path(get, path = "/library/stats", tag = "Search", responses(
    (status = 200, body = crate::library::browse::LibraryStats,
        description = "Shared catalog overview"),
    (status = 401, response = ErrorResponse),
    (status = 500, response = ErrorResponse)
))]
async fn stats(
    State(pool): State<SqlitePool>,
) -> Result<Json<crate::library::browse::LibraryStats>, ApiError> {
    Ok(Json(db::browse::stats(&pool).await?))
}

/// Apply authentication to catalog GET/HEAD before query/path extraction.
/// Mutating handlers retain their own independent administrator guards.
pub(super) fn protect(
    router: OpenApiRouter,
    pool: SqlitePool,
    public: bool,
) -> OpenApiRouter {
    router.layer(middleware::from_fn_with_state(
        (pool, public),
        catalog_access,
    ))
}

/// Avoid public caches for all private catalog outcomes, including errors.
async fn catalog_access(
    State((pool, public)): State<(SqlitePool, bool)>,
    request: Request,
    next: Next,
) -> Response {
    if public || !matches!(*request.method(), Method::GET | Method::HEAD) {
        return next.run(request).await;
    }
    let (mut parts, body) = request.into_parts();
    let mut response =
        match CurrentUser::from_request_parts(&mut parts, &pool).await {
            Ok(_) => next.run(Request::from_parts(parts, body)).await,
            Err(error) => error.into_response(),
        };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().append(
        header::VARY,
        axum::http::HeaderValue::from_static("Authorization"),
    );
    response
}

/// Enable browser access only for configured exact origins, without cookies.
pub(super) fn cors(router: Router, settings: &ClientSettings) -> Router {
    if settings.cors_origins.is_empty() {
        return router;
    }
    // Configuration validation guarantees valid header values. Invalid
    // internal fixtures fail closed instead of adding a wildcard.
    let origins: Vec<_> = settings
        .cors_origins
        .iter()
        .filter_map(|origin| origin.parse().ok())
        .collect();
    router.layer(
        tower_http::cors::CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
                header::RANGE,
                header::IF_RANGE,
                header::IF_NONE_MATCH,
            ])
            .expose_headers([
                header::CONTENT_LENGTH,
                header::CONTENT_RANGE,
                header::ACCEPT_RANGES,
                header::ETAG,
                header::LOCATION,
                header::RETRY_AFTER,
                header::WWW_AUTHENTICATE,
                header::ALLOW,
            ])
            .max_age(std::time::Duration::from_secs(600)),
    )
}

#[cfg(test)]
#[path = "clients_tests.rs"]
mod tests;
