//! HTTP routing and process health endpoint.

pub(crate) mod error;

use axum::{Json, Router, routing::get};
use serde::Serialize;

use self::error::ApiError;

/// Build the HTTP API without binding a network listener.
///
/// Register all routes before the method fallback so unsupported methods on
/// every registered path receive the same JSON error envelope.
pub(crate) fn router() -> Router {
    Router::new()
        .route("/health", get(health))
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
#[derive(Serialize)]
struct Health {
    /// Fixed status indicating the HTTP handler is available.
    status: &'static str,
}

/// Report that the process can handle HTTP requests.
async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

#[cfg(test)]
mod tests {
    //! Router-level checks without opening network sockets.

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;

    use super::*;

    /// Health returns HTTP 200 with the documented JSON response.
    #[tokio::test]
    async fn health_returns_ok_json() {
        let response = router()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
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
                        .uri("/health")
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
                    .uri("/health")
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
}
