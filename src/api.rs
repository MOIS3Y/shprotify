//! HTTP routing and process health endpoint.

use axum::{Json, Router, routing::get};
use serde::Serialize;

/// Build the HTTP API without binding a network listener.
pub(crate) fn router() -> Router {
    Router::new().route("/health", get(health))
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

    /// Unregistered paths are not reported as healthy.
    #[tokio::test]
    async fn unknown_route_returns_not_found() {
        let response = router()
            .oneshot(Request::get("/missing").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
