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

Sections follow the client workflow: sign in, browse the catalog, play music,
manage personal collections, discover music and acquire selected tracks.
Sections prefixed `Admin:` require the administrator role. Catalog reads,
search and artwork follow `library.public_catalog`.
Playback and personal data require
an active Bearer credential. Each operation documents its own permissions.

Library scans and shared catalog maintenance are under `Admin: Library`
and `Admin: Catalog`. Metadata corrections and external proposal review are
separate workflows. Personal feed schedules stay with `Discovery`; global
policy and metadata schedules are under `Admin: Automation`.

Errors use a JSON envelope with `error.code` and `error.message`:
- 400: `bad_request` — Malformed request or invalid parameters.
- 401: `unauthorized` — Invalid/expired credential; Bearer challenge.
- 403: `forbidden` — Administrator role required.
- 413: `payload_too_large` — Request body exceeds the size limit.
- 415: `unsupported_media_type` — JSON content type is required.
- 416: `range_not_satisfiable` — Requested file bytes are unavailable.
- 422: `validation_error` — Invalid JSON fields or domain input.
- 429: `too_many_requests` — Password workers busy; includes `Retry-After`.
- 404: `not_found` — Resource not found.
- 405: `method_not_allowed` — Method not allowed; includes `Allow`.
- 409: `conflict` — Catalog state conflicts with the requested change.
- 500: `internal_error` — Internal server error.

Internal failure details are logged on the server and never returned.
HEAD responses have no body, including error responses."
    ),
    tags(
        (name = "Authentication", description =
            "Password login and session logout."),
        (name = "Account", description =
            "Current account identity and role."),
        (name = "Search", description =
            "Search the shared music catalog."),
        (name = "Artists", description =
            "Browse artists in the shared catalog."),
        (name = "Releases", description =
            "Browse releases, credits and ordered track lists."),
        (name = "Recordings", description =
            "Browse recordings, independent of releases and files."),
        (name = "Audio files", description =
            "Registered formats, measured properties and source tags."),
        (name = "Artwork", description =
            "Catalog covers and cache validators; catalog policy applies."),
        (name = "Playback", description =
            "Authenticated audio delivery and byte-range seeking."),
        (name = "Queue", description =
            "Personal persistent queue and playback position."),
        (name = "Favorites", description =
            "Personal recording reactions and dynamic favorites."),
        (name = "History", description =
            "Completed playback facts and personal history."),
        (name = "Playlists", description =
            "Owned playlists and playlists published in the instance."),
        (name = "Discovery", description =
            "Preferences, feeds, decisions, wishlist and feed schedules."),
        (name = "Downloads", description =
            "Experimental source search, acquisition and job progress."),
        (name = "Admin: Users", description =
            "Administrator only: accounts and credentials."),
        (name = "Admin: Catalog", description =
            "Administrator only: catalog creation and file registration."),
        (name = "Admin: Library", description =
            "Administrator only: scans, reports and duplicate inspection."),
        (name = "Admin: Metadata", description =
            "Administrator only: corrections, revisions, covers and links."),
        (name = "Admin: Enrichment", description =
            "Administrator only: lookups, identification and reviews."),
        (name = "Admin: Automation", description =
            "Administrator only: global policy and metadata schedules."),
        (name = "System", description =
            "Process health.")
    ),
    components(schemas(ErrorResponse), responses(ErrorResponse)),
    modifiers(&PersonalSecurity)
)]
pub(super) struct ApiDoc;

/// Shared Bearer scheme for personal keys and password session tokens.
struct PersonalSecurity;

impl utoipa::Modify for PersonalSecurity {
    /// Register bearer credentials without imposing security on public routes.
    fn modify(&self, api: &mut openapi::OpenApi) {
        use openapi::security::{
            ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme,
        };
        if let Some(components) = &mut api.components {
            components.add_security_scheme(
                "media_ticket",
                SecurityScheme::ApiKey(ApiKey::Query(ApiKeyValue::new(
                    "ticket",
                ))),
            );
            components.add_security_scheme(
                "personal_key",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
        }
    }
}

/// Document the actual startup policy for every catalog read section.
/// Media and administrator operations keep their independent security rules.
pub(super) fn catalog_policy(api: &mut openapi::OpenApi, public: bool) {
    const CATALOG: [&str; 6] = [
        "Search",
        "Artists",
        "Releases",
        "Recordings",
        "Audio files",
        "Artwork",
    ];
    for path in api.paths.paths.values_mut() {
        for operation in [&mut path.get, &mut path.head].into_iter().flatten()
        {
            let catalog = operation.tags.as_ref().is_some_and(|tags| {
                tags.iter().any(|tag| CATALOG.contains(&tag.as_str()))
            });
            if !catalog {
                continue;
            }
            operation.responses.responses.insert(
                "401".into(),
                openapi::Ref::new("#/components/responses/ErrorResponse")
                    .into(),
            );
            if !public {
                operation.security =
                    Some(vec![openapi::security::SecurityRequirement::new(
                        "personal_key",
                        std::iter::empty::<String>(),
                    )]);
            }
        }
    }
}

/// Add Swagger UI and OpenAPI JSON only when enabled at startup.
pub(super) fn attach(
    router: Router,
    schema: openapi::OpenApi,
    config: &DocsConfig,
) -> Router {
    if config.enabled {
        router.merge(
            SwaggerUi::new("/docs").url("/openapi.json", schema).config(
                SwaggerConfig::default()
                    .validator_url("none")
                    .filter(true)
                    .doc_expansion("none")
                    .deep_linking(true),
            ),
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
            &crate::config::AuthConfig::default(),
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
        assert!(script.contains(r#""filter": true"#));
        assert!(script.contains(r#""docExpansion": "none""#));
        assert!(script.contains(r#""deepLinking": true"#));

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
        let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let schema: Value = serde_json::from_slice(&body).unwrap();
        assert!(schema["openapi"].as_str().unwrap().starts_with("3."));
        assert_eq!(schema["info"]["title"], "Shprotify API");
        assert_eq!(schema["info"]["version"], env!("CARGO_PKG_VERSION"));
        let operation = &schema["paths"]["/api/v1/health"]["get"];
        assert_eq!(operation["operationId"], "get_health");
        assert_eq!(operation["tags"][0], "System");
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
        assert_eq!(schema["paths"].as_object().unwrap().len(), 84);
        assert!(schema["paths"].get("/health").is_none());
    }

    /// Stable workflow sections prevent duplicate operations and hidden admin
    /// scope.
    #[tokio::test]
    async fn schema_groups_client_workflows_before_administration() {
        use std::collections::HashSet;

        let response = get("/openapi.json", true).await;
        let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let schema: Value = serde_json::from_slice(&bytes).unwrap();
        let sections: Vec<_> = schema["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tag| tag["name"].as_str().unwrap())
            .collect();
        assert_eq!(sections.first(), Some(&"Authentication"));
        assert_eq!(sections.get(1), Some(&"Account"));
        assert_eq!(sections.last(), Some(&"System"));
        let admin_start = sections
            .iter()
            .position(|tag| tag.starts_with("Admin:"))
            .unwrap();
        assert!(
            sections[admin_start..sections.len() - 1]
                .iter()
                .all(|tag| tag.starts_with("Admin:"))
        );
        let declared: HashSet<_> = sections.iter().copied().collect();
        assert_eq!(declared.len(), sections.len());
        let mut used = HashSet::new();
        let mut operation_ids = HashSet::new();
        for methods in schema["paths"].as_object().unwrap().values() {
            for method in ["get", "put", "post", "delete", "patch", "head"] {
                let Some(operation) = methods.get(method) else {
                    continue;
                };
                let identity = operation["operationId"].as_str().unwrap();
                assert!(
                    operation_ids.insert(identity),
                    "Duplicate operationId {identity} breaks client generation"
                );
                let tags = operation["tags"].as_array().unwrap();
                assert_eq!(
                    tags.len(),
                    1,
                    "Every operation belongs to one section"
                );
                let section = tags[0].as_str().unwrap();
                assert!(
                    declared.contains(section),
                    "Undeclared section {section}"
                );
                used.insert(section);
                if section.starts_with("Admin:") {
                    assert!(
                        operation["security"][0]["personal_key"].is_array()
                    );
                    assert!(operation["responses"]["401"].is_object());
                    assert!(operation["responses"]["403"].is_object());
                }
            }
        }
        assert_eq!(used, declared, "No empty future or forgotten sections");
        for (path, method, section) in [
            ("/artists", "get", "Artists"),
            ("/artists", "post", "Admin: Catalog"),
            ("/recordings/{id}/files/{file_id}/stream", "get", "Playback"),
            ("/artwork/{kind}/{id}", "get", "Artwork"),
            (
                "/metadata/candidates/{id}/artwork",
                "get",
                "Admin: Enrichment",
            ),
            ("/me/discovery/feeds/{id}/schedule", "put", "Discovery"),
            ("/metadata/{kind}/{id}/schedule", "put", "Admin: Automation"),
        ] {
            assert_eq!(
                schema["paths"][format!("/api/v1{path}")][method]["tags"][0],
                section
            );
        }
    }

    /// Artist operations expose paging, request schemas, and shared errors.
    #[tokio::test]
    async fn schema_contains_artist_contracts() {
        let response = get("/openapi.json", true).await;
        let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
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
        let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
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
        assert_eq!(list["parameters"].as_array().unwrap().len(), 8);
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
        let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
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
        assert_eq!(list["parameters"].as_array().unwrap().len(), 7);
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
        let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
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
        let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
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
