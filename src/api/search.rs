//! Unified, bounded catalog search with literal user text and typed hits.

use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    db,
    library::search::{self, SearchHit, SearchKind},
};

/// Register search within the single versioned API prefix.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(search_catalog))
        .with_state(pool)
}

/// Required phrase plus optional category and bounded page selection.
#[derive(Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
struct SearchQuery {
    /// Literal phrase; its final token can be a prefix, e.g. `Massive Att`.
    #[param(min_length = 1, max_length = 255)]
    q: String,
    /// Omitted means search all three catalog categories together.
    kind: Option<SearchKind>,
    /// Maximum combined hits; between 1 and 100, default 50.
    #[param(minimum = 1, maximum = 100, default = 50)]
    limit: Option<i64>,
    /// Number of combined hits to skip; nonnegative, default zero.
    #[param(minimum = 0, default = 0)]
    offset: Option<i64>,
}

/// Combined hit page ordered by relevance and stable category/ID tie breakers.
#[derive(Serialize, ToSchema)]
struct SearchPage {
    /// Matching identities; equal display names can have different IDs.
    items: Vec<SearchHit>,
    /// Effective maximum combined hits.
    limit: i64,
    /// Number of skipped hits.
    offset: i64,
}

/// Search names/titles with Unicode case-insensitive tokens and prefixes.
///
/// Literal tokens must be adjacent and ordered, with a prefix on the last one.
/// Punctuation follows SQLite unicode61 tokenization; accents are preserved.
/// FTS operators are treated literally. Paths, source tags, credits, and audio
/// contents are not searchable fields. Files need not exist for a catalog hit.
#[utoipa::path(
    get, path = "/search", tag = "search", operation_id = "search_catalog",
    params(SearchQuery), responses(
        (status = 200, body = SearchPage, description = "Ranked catalog hits"),
        (status = 400, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn search_catalog(
    State(pool): State<SqlitePool>,
    query: Result<Query<SearchQuery>, QueryRejection>,
) -> Result<Json<SearchPage>, ApiError> {
    let Query(query) =
        query.map_err(|_| ApiError::BadRequest("Invalid query"))?;
    let phrase = search::phrase(&query.q).map_err(ApiError::BadRequest)?;
    let defaults = Pagination::default();
    let page = Pagination {
        limit: query.limit.unwrap_or(defaults.limit),
        offset: query.offset.unwrap_or(defaults.offset),
    };
    page.validate()?;
    let items =
        db::search::find(&pool, &phrase, query.kind, page.limit, page.offset)
            .await?;
    Ok(Json(SearchPage {
        items,
        limit: page.limit,
        offset: page.offset,
    }))
}

#[cfg(test)]
mod tests {
    //! Router-level validation, result schemas, and safe storage failures.

    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Send a request through the complete API without binding a socket.
    async fn get(app: &Router, path: &str) -> (u16, Value) {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// Literal queries, pagination, errors, and OpenAPI match actual routing.
    #[tokio::test]
    async fn searches_and_validates_the_versioned_http_contract() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        sqlx::query("INSERT INTO artists (name) VALUES ('Massive Attack')")
            .execute(&pool)
            .await
            .unwrap();
        let app = crate::api::router(
            &crate::config::DocsConfig { enabled: true },
            &pool,
            crate::jobs::ScanService::new(pool.clone(), vec![]),
        );
        let (status, page) =
            get(&app, "/api/v1/search?q=massive+att&kind=artist&limit=1")
                .await;
        assert_eq!(status, 200);
        assert_eq!(
            page["items"][0],
            json!({"kind": "artist", "id": 1,
            "title": "Massive Attack"})
        );
        assert_eq!(page["limit"], 1);
        let (_, empty) = get(&app, "/api/v1/search?q=massive&offset=1").await;
        assert_eq!(empty["items"], json!([]));
        for query in [
            "",
            "q=",
            "q=+",
            "q=x&kind=unknown",
            "q=x&limit=0",
            "q=x&limit=101",
            "q=x&offset=-1",
            "q=x&unknown=1",
            "q=a%00b",
            "q=x&limit=no",
            "q=x&q=y",
        ] {
            let (status, error) =
                get(&app, &format!("/api/v1/search?{query}")).await;
            assert_eq!(status, 400, "{query}");
            assert_eq!(error["error"]["code"], "bad_request");
        }
        let (_, schema) = get(&app, "/openapi.json").await;
        let operation = &schema["paths"]["/api/v1/search"]["get"];
        assert_eq!(operation["operationId"], "search_catalog");
        assert!(operation["responses"]["400"].is_object());
        assert!(schema["components"]["schemas"]["SearchHit"].is_object());
        pool.close().await;
        let (status, error) = get(&app, "/api/v1/search?q=test").await;
        assert_eq!(status, 500);
        assert_eq!(error["error"]["message"], "Internal server error");
    }
}
