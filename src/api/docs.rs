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
- 400: `bad_request` — Malformed request or invalid parameters.
- 413: `payload_too_large` — Request body exceeds the size limit.
- 415: `unsupported_media_type` — JSON content type is required.
- 422: `validation_error` — Invalid JSON fields or domain input.
- 404: `not_found` — Resource not found.
- 405: `method_not_allowed` — Method not allowed; includes `Allow`.
- 409: `conflict` — Catalog state conflicts with the requested change.
- 500: `internal_error` — Internal server error.

Internal failure details are logged on the server and never returned.
HEAD responses have no body, including error responses."
    ),
    tags(
        (name = "system", description = "Process health"),
        (name = "artists", description = "Shared artist catalog"),
        (name = "releases", description = "Releases and artist credits"),
        (name = "recordings", description = "Independent musical recordings"),
        (name = "release_tracks", description = "Tracks within releases"),
        (name = "audio_files", description = "File metadata and source tags"),
        (name = "library_scans", description = "Background library indexing")
    ),
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
        let pool = sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap();
        crate::api::router(
            &DocsConfig { enabled },
            &pool,
            crate::jobs::ScanService::new(pool.clone(), vec![]),
        )
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
        assert_eq!(schema["paths"].as_object().unwrap().len(), 16);
        assert!(schema["paths"].get("/health").is_none());
    }

    /// Artist operations expose paging, request schemas, and shared errors.
    #[tokio::test]
    async fn schema_contains_artist_contracts() {
        let response = get("/openapi.json", true).await;
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let schema: Value = serde_json::from_slice(&body).unwrap();
        let paths = &schema["paths"];
        let create = &paths["/api/v1/artists"]["post"];
        assert_eq!(create["operationId"], "create_artist");
        assert!(
            create["requestBody"]["content"]["application/json"].is_object()
        );
        assert!(create["responses"]["201"]["headers"]["Location"].is_object());
        for status in ["400", "413", "415", "422", "500"] {
            assert_eq!(
                create["responses"][status]["$ref"],
                "#/components/responses/ErrorResponse"
            );
        }
        let list = &paths["/api/v1/artists"]["get"];
        assert_eq!(list["operationId"], "list_artists");
        let parameters = list["parameters"].as_array().unwrap();
        let limit = parameters
            .iter()
            .find(|parameter| parameter["name"] == "limit")
            .unwrap();
        assert_eq!(limit["schema"]["default"], 50);
        assert_eq!(limit["schema"]["maximum"], 100);
        assert_eq!(
            paths["/api/v1/artists/{id}"]["get"]["responses"]["404"]["$ref"],
            "#/components/responses/ErrorResponse"
        );
        let artist = &schema["components"]["schemas"]["Artist"];
        assert_eq!(artist["properties"]["id"]["type"], "integer");
        assert_eq!(artist["properties"]["name"]["type"], "string");
        assert!(paths.get("/artists").is_none());
    }

    /// Release operations describe ordered credits, kinds, and error statuses.
    #[tokio::test]
    async fn schema_contains_release_contracts() {
        let response = get("/openapi.json", true).await;
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let schema: Value = serde_json::from_slice(&body).unwrap();
        let paths = &schema["paths"];
        let create = &paths["/api/v1/releases"]["post"];
        assert_eq!(create["operationId"], "create_release");
        assert!(create["responses"]["201"]["headers"]["Location"].is_object());
        for status in ["400", "413", "415", "422", "500"] {
            assert_eq!(
                create["responses"][status]["$ref"],
                "#/components/responses/ErrorResponse"
            );
        }
        let list = &paths["/api/v1/releases"]["get"];
        assert_eq!(list["operationId"], "list_releases");
        assert_eq!(list["parameters"].as_array().unwrap().len(), 2);
        let detail = &paths["/api/v1/releases/{id}"]["get"];
        assert_eq!(detail["operationId"], "get_release");
        assert!(detail["responses"]["404"].is_object());
        let schemas = &schema["components"]["schemas"];
        assert_eq!(
            schemas["Release"]["properties"]["artist_ids"]["type"],
            "array"
        );
        assert_eq!(
            schemas["ReleaseKind"]["enum"],
            serde_json::json!([
                "album",
                "single",
                "ep",
                "compilation",
                "other"
            ])
        );
        assert!(paths.get("/releases").is_none());
    }

    /// Recording contracts expose independent identity and ordered credits.
    #[tokio::test]
    async fn schema_contains_independent_recording_contracts() {
        let response = get("/openapi.json", true).await;
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let schema: Value = serde_json::from_slice(&body).unwrap();
        let paths = &schema["paths"];
        let create = &paths["/api/v1/recordings"]["post"];
        assert_eq!(create["operationId"], "create_recording");
        assert!(create["responses"]["201"]["headers"]["Location"].is_object());
        for status in ["400", "413", "415", "422", "500"] {
            assert_eq!(
                create["responses"][status]["$ref"],
                "#/components/responses/ErrorResponse"
            );
        }
        let list = &paths["/api/v1/recordings"]["get"];
        assert_eq!(list["operationId"], "list_recordings");
        assert_eq!(list["parameters"].as_array().unwrap().len(), 2);
        let detail = &paths["/api/v1/recordings/{id}"]["get"];
        assert_eq!(detail["operationId"], "get_recording");
        assert!(detail["responses"]["404"].is_object());
        let schemas = &schema["components"]["schemas"];
        let recording = &schemas["Recording"]["properties"];
        assert_eq!(recording["id"]["type"], "integer");
        assert_eq!(recording["artist_ids"]["type"], "array");
        let input = &schemas["CreateRecording"]["properties"];
        assert!(input.get("release_id").is_none());
        assert!(input.get("file_path").is_none());
        assert!(input.get("kind").is_none());
        assert!(paths.get("/recordings").is_none());
    }

    /// Nested track routes document defaults, parent IDs, and slot conflicts.
    #[tokio::test]
    async fn schema_contains_release_track_contracts() {
        let response = get("/openapi.json", true).await;
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let schema: Value = serde_json::from_slice(&body).unwrap();
        let paths = &schema["paths"];
        let collection = &paths["/api/v1/releases/{id}/tracks"];
        let create = &collection["post"];
        assert_eq!(create["operationId"], "create_release_track");
        assert!(create["responses"]["201"]["headers"]["Location"].is_object());
        for status in ["400", "404", "409", "413", "415", "422", "500"] {
            assert_eq!(
                create["responses"][status]["$ref"],
                "#/components/responses/ErrorResponse"
            );
        }
        let list = &collection["get"];
        assert_eq!(list["operationId"], "list_release_tracks");
        assert_eq!(list["parameters"].as_array().unwrap().len(), 3);
        let detail = &paths["/api/v1/releases/{id}/tracks/{track_id}"]["get"];
        assert_eq!(detail["operationId"], "get_release_track");
        assert_eq!(detail["parameters"].as_array().unwrap().len(), 2);
        let schemas = &schema["components"]["schemas"];
        let input = &schemas["CreateTrack"];
        assert_eq!(input["properties"]["disc_number"]["default"], 1);
        assert!(
            !input["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("disc_number"))
        );
        let track = &schemas["ReleaseTrack"]["properties"];
        for field in [
            "id",
            "release_id",
            "recording_id",
            "disc_number",
            "position",
        ] {
            assert_eq!(track[field]["type"], "integer");
            assert_eq!(track[field]["minimum"], 1);
        }
    }

    /// File routes describe registration, optional quality, and parent scope.
    #[tokio::test]
    async fn schema_contains_audio_file_metadata_contracts() {
        let response = get("/openapi.json", true).await;
        let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let schema: Value = serde_json::from_slice(&body).unwrap();
        let paths = &schema["paths"];
        let collection = &paths["/api/v1/recordings/{id}/files"];
        let create = &collection["post"];
        assert_eq!(create["operationId"], "register_audio_file");
        assert!(create["responses"]["201"]["headers"]["Location"].is_object());
        for status in ["400", "404", "409", "413", "415", "422", "500"] {
            assert_eq!(
                create["responses"][status]["$ref"],
                "#/components/responses/ErrorResponse"
            );
        }
        let list = &collection["get"];
        assert_eq!(list["operationId"], "list_audio_files");
        assert_eq!(list["parameters"].as_array().unwrap().len(), 3);
        let detail = &paths["/api/v1/recordings/{id}/files/{file_id}"]["get"];
        assert_eq!(detail["operationId"], "get_audio_file");
        let schemas = &schema["components"]["schemas"];
        let input = &schemas["RegisterFile"];
        assert!(
            !input["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("properties"))
        );
        assert!(input["properties"].get("recording_id").is_none());
        assert_eq!(
            schemas["AudioFile"]["properties"]["properties"]["$ref"],
            "#/components/schemas/AudioProperties"
        );
        for field in [
            "size_bytes",
            "duration_ms",
            "sample_rate_hz",
            "channels",
            "bit_depth",
            "bitrate_bps",
        ] {
            assert_eq!(
                schemas["AudioProperties"]["properties"][field]["minimum"],
                1
            );
        }
    }
}
