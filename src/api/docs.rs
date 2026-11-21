//! OpenAPI metadata and optional, embedded Swagger UI routes.
//!
//! Keep operation annotations beside handlers and register them through
//! `utoipa_axum::routes!` so routing and documentation share one definition.
//! The startup setting controls both the UI and JSON schema; no runtime
//! assets directory or network download is needed.

use axum::Router;
use utoipa::{OpenApi, openapi};
use utoipa_swagger_ui::{Config as SwaggerConfig, SwaggerUi};

use crate::config::DocsConfig;

use super::error::ErrorResponse;

/// Shared API metadata and error schemas; routes supply their own operations.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Shprotify API",
        description = "Self-hosted music server HTTP API, version 1.
All API operations are served under `/api/v1`.

Errors use a JSON envelope with `error.code` and `error.message`:
- 404: `not_found` — Resource not found.
- 405: `method_not_allowed` — Method not allowed; includes `Allow`.
- 500: `internal_error` — Internal server error.

Internal failure details are logged on the server and never returned.
HEAD responses have no body, including error responses."
    ),
    tags((name = "system", description = "Process health")),
    components(schemas(ErrorResponse), responses(ErrorResponse))
)]
pub(super) struct ApiDoc;

/// Add Swagger UI and OpenAPI JSON only when enabled at startup.
pub(super) fn attach(
    router: Router,
    schema: openapi::OpenApi,
    config: &DocsConfig,
) -> Router {
    if config.enabled {
        router.merge(
            SwaggerUi::new("/docs")
                .url("/openapi.json", schema)
                .config(SwaggerConfig::default().validator_url("none")),
        )
    } else {
        router
    }
}

#[cfg(test)]
mod tests {
    //! Documentation visibility, bundled assets, and generated API contracts.

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
        response::Response,
    };
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    /// Send a GET through the complete router without opening a socket.
    async fn get(path: &str, enabled: bool) -> Response {
        crate::api::router(&DocsConfig { enabled })
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// Disabling documentation hides its schema, UI, and bundled assets.
    #[tokio::test]
    async fn disabled_docs_return_api_not_found() {
        for path in [
            "/docs",
            "/docs/",
            "/docs/swagger-ui-bundle.js",
            "/docs/swagger-initializer.js",
            "/openapi.json",
        ] {
            let response = get(path, false).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["error"]["code"], "not_found");
        }
    }

    /// Swagger serves HTML and local assets referencing our schema URL.
    #[tokio::test]
    async fn enabled_docs_serve_ui_and_assets() {
        let redirect = get("/docs", true).await;
        assert!(redirect.status().is_redirection());
        assert_eq!(redirect.headers()[header::LOCATION], "/docs/");

        let response = get("/docs/", true).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let html = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        assert!(std::str::from_utf8(&html).unwrap().contains("Swagger UI"));

        let response = get("/docs/swagger-initializer.js", true).await;
        assert_eq!(response.status(), StatusCode::OK);
        let script = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let script = std::str::from_utf8(&script).unwrap();
        assert!(script.contains("/openapi.json"));
        assert!(script.contains(r#""validatorUrl": "none""#));

        let response = get("/docs/swagger-ui-bundle.js", true).await;
        assert_eq!(response.status(), StatusCode::OK);
        let script = to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await
            .unwrap();
        assert!(!script.is_empty());
    }

    /// Generated metadata and schemas describe the actual health contract.
    #[tokio::test]
    async fn schema_contains_health_and_shared_errors() {
        let response = get("/openapi.json", true).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json"
        );
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let schema: Value = serde_json::from_slice(&body).unwrap();
        assert!(schema["openapi"].as_str().unwrap().starts_with("3."));
        assert_eq!(schema["info"]["title"], "Shprotify API");
        assert_eq!(schema["info"]["version"], env!("CARGO_PKG_VERSION"));
        let operation = &schema["paths"]["/api/v1/health"]["get"];
        assert_eq!(operation["operationId"], "get_health");
        assert_eq!(operation["tags"][0], "system");
        let response = &operation["responses"]["200"]["content"];
        assert_eq!(
            response["application/json"]["schema"]["$ref"],
            "#/components/schemas/Health"
        );
        let schemas = &schema["components"]["schemas"];
        assert_eq!(
            schemas["Health"]["properties"]["status"]["type"],
            "string"
        );
        assert_eq!(
            schemas["ErrorResponse"]["properties"]["error"]["$ref"],
            "#/components/schemas/ErrorBody"
        );
        assert!(schemas["ErrorBody"]["properties"]["code"].is_object());
        assert!(schemas["ErrorBody"]["properties"]["message"].is_object());
        let response = &schema["components"]["responses"]["ErrorResponse"];
        assert!(response["description"].is_string());
        let content = &response["content"]["application/json"]["schema"];
        assert_eq!(
            content["properties"]["error"]["$ref"],
            "#/components/schemas/ErrorBody"
        );
        assert_eq!(schema["paths"].as_object().unwrap().len(), 1);
        assert!(schema["paths"].get("/health").is_none());
    }
}
