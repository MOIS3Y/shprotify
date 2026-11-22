//! Versioned HTTP routing and process health endpoint.

mod artists;
mod audio_files;
mod docs;
mod duplicates;
pub(crate) mod error;
mod pagination;
#[cfg(test)]
mod permissions;
mod personal;
mod recordings;
mod release_tracks;
mod releases;
mod scans;
mod search;
mod sessions;
mod streaming;
mod users;

use axum::{Json, Router};
use serde::Serialize;
use utoipa::{OpenApi, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::{config::DocsConfig, jobs::ScanService};

use self::error::ApiError;

/// Build the HTTP API without binding a network listener.
///
/// Register all routes before the method fallback so unsupported methods on
/// every registered path receive the same JSON error envelope.
/// Version-local paths are nested once under `/api/v1` for both HTTP routing
/// and OpenAPI. Documentation routes are attached at the application root.
pub(crate) fn router(
    auth: &crate::config::AuthConfig,
    config: &DocsConfig,
    pool: &sqlx::SqlitePool,
    scans: ScanService,
) -> Router {
    let v1 = OpenApiRouter::new()
        .routes(routes!(health))
        .merge(streaming::router(pool.clone(), scans.roots()))
        .merge(sessions::router(pool.clone(), *auth))
        .merge(personal::router(pool.clone()))
        .merge(users::router(pool.clone()))
        .merge(artists::router(pool.clone()))
        .merge(releases::router(pool.clone()))
        .merge(recordings::router(pool.clone()))
        .merge(release_tracks::router(pool.clone()))
        .merge(audio_files::router(pool.clone()))
        .merge(duplicates::router(pool.clone()))
        .merge(search::router(pool.clone()))
        .merge(scans::router(pool.clone(), scans));
    let (router, schema) =
        OpenApiRouter::with_openapi(docs::ApiDoc::openapi())
            .nest("/api/v1", v1)
            .split_for_parts();

    docs::attach(router, schema, config)
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
}

/// Return a JSON 404 for paths that do not match any API route.
async fn not_found() -> ApiError {
    ApiError::NotFound
}

/// Return a JSON 405; Axum supplies the route's allowed methods header.
async fn method_not_allowed() -> ApiError {
    ApiError::MethodNotAllowed
}

/// Process liveness response; storage readiness is not checked here.
#[derive(Serialize, ToSchema)]
struct Health {
    /// Fixed status indicating the HTTP handler is available.
    #[schema(example = "ok")]
    status: &'static str,
}

/// Report that the process can handle HTTP requests.
///
/// This liveness check does not query the database or external services.
#[utoipa::path(
    get,
    path = "/health",
    tag = "system",
    operation_id = "get_health",
    responses((status = 200, description = "Process is alive", body = Health))
)]
async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

#[cfg(test)]
pub(crate) mod tests {
    //! Router-level checks without opening network sockets.

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
        routing::get,
    };
    use tower::ServiceExt;

    use super::*;

    /// Authenticate catalog contract fixtures with a real administrator key.
    /// Authorization-specific tests use the unmodified production router.
    pub(crate) async fn with_admin(
        app: Router,
        pool: &sqlx::SqlitePool,
    ) -> Router {
        let token = crate::auth::token().unwrap();
        let name = format!("test-{}", &token[4..20]);
        crate::db::users::create(pool, &name, &crate::auth::digest(&token))
            .await
            .unwrap();
        crate::db::users::set_role(pool, &name, crate::auth::Role::Admin)
            .await
            .unwrap();
        app.layer(axum::middleware::from_fn(
            move |mut request: Request<Body>, next: axum::middleware::Next| {
                let token = token.clone();
                async move {
                    request.headers_mut().insert(
                        header::AUTHORIZATION,
                        format!("Bearer {token}").parse().unwrap(),
                    );
                    next.run(request).await
                }
            },
        ))
    }

    /// Build the default router with all documentation routes disabled.
    fn router() -> Router {
        super::router(
            &crate::config::AuthConfig::default(),
            &DocsConfig::default(),
            &test_pool(),
            ScanService::new(test_pool(), vec![]),
        )
    }

    /// Supply unused state for routes that do not access the database.
    fn test_pool() -> sqlx::SqlitePool {
        sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap()
    }

    /// Health returns HTTP 200 with the documented JSON response.
    #[tokio::test]
    async fn health_returns_ok_json() {
        let response = router()
            .oneshot(
                Request::get("/api/v1/health").body(Body::empty()).unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json"
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body.as_ref(), br#"{"status":"ok"}"#);
    }

    /// Unknown paths return JSON 404 regardless of the requested method.
    #[tokio::test]
    async fn unknown_route_returns_not_found() {
        for method in ["GET", "POST", "OPTIONS"] {
            let response = router()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/missing")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/json"
            );
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(
                body.as_ref(),
                concat!(
                    r#"{"error":{"code":"not_found","#,
                    r#""message":"Resource not found"}}"#
                )
                .as_bytes()
            );
        }
    }

    /// Unsupported methods receive JSON 405 with an accurate Allow header.
    #[tokio::test]
    async fn unsupported_method_returns_json_and_allowed_methods() {
        for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
            let response = router()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/api/v1/health")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[header::ALLOW], "GET,HEAD");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/json"
            );
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(
                body.as_ref(),
                concat!(
                    r#"{"error":{"code":"method_not_allowed","#,
                    r#""message":"Method not allowed"}}"#
                )
                .as_bytes()
            );
        }
    }

    /// HEAD remains supported for health and returns no response body.
    #[tokio::test]
    async fn head_health_remains_supported() {
        let response = router()
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert!(body.is_empty());
    }

    /// A fallible handler uses the shared error response through Result.
    #[tokio::test]
    async fn internal_handler_error_returns_safe_json() {
        let app = router().route("/failure", get(failing_handler));
        let response = app
            .oneshot(Request::get("/failure").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json"
        );
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(
            body.as_ref(),
            concat!(
                r#"{"error":{"code":"internal_error","#,
                r#""message":"Internal server error"}}"#
            )
            .as_bytes()
        );
    }

    /// Simulate a failing dependency without exposing a production endpoint.
    async fn failing_handler() -> Result<(), ApiError> {
        Err(anyhow::anyhow!("database password=secret").into())
    }

    /// Only v1 API paths exist, independently of documentation visibility.
    #[tokio::test]
    async fn unversioned_and_unknown_api_paths_return_json_not_found() {
        for enabled in [false, true] {
            let app = super::router(
                &crate::config::AuthConfig::default(),
                &DocsConfig { enabled },
                &test_pool(),
                ScanService::new(test_pool(), vec![]),
            );
            for path in [
                "/health",
                "/api/v1",
                "/api/v1/",
                "/api/v1/missing",
                "/api/v2/health",
                "/api/v1/docs",
                "/api/v1/openapi.json",
            ] {
                let response = app
                    .clone()
                    .oneshot(Request::get(path).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                let error: serde_json::Value =
                    serde_json::from_slice(&body).unwrap();
                assert_eq!(error["error"]["code"], "not_found", "{path}");
            }
        }
    }
}
