//! Paginated reports of exact file duplicates without automatic catalog edits.

use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
};
use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::db::{self, duplicates::DuplicateFile};

/// Register the duplicate report with the existing versioned API.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(list_duplicates))
        .with_state(pool)
}

/// A file-bounded page; a digest group can span multiple pages.
#[derive(Serialize, ToSchema)]
struct DuplicatePage {
    /// Files with shared full-content digests, ordered by digest and ID.
    items: Vec<DuplicateFile>,
    /// Maximum files in this page.
    limit: i64,
    /// Number of matching files skipped.
    offset: i64,
}

/// Report byte-identical files from their last successful background probes.
///
/// Hashes include tags and container bytes. Different encodings or retagged
/// files are not identified as equivalent recordings. Paths may be missing
/// since their last observation. No files or catalog identities are changed.
///
/// Requires the administrator role.
#[utoipa::path(
    get, path = "/library/duplicates", tag = "library_scans",
    operation_id = "list_duplicate_files", params(Pagination),
    security(("personal_key" = [])),
    responses(
        (status = 200, body = DuplicatePage, description = "Duplicate files"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    )
)]
async fn list_duplicates(
    _admin: crate::auth::Admin,
    State(pool): State<SqlitePool>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<DuplicatePage>, ApiError> {
    let Query(page) =
        query.map_err(|_| ApiError::BadRequest("Invalid query"))?;
    page.validate()?;
    let items = db::duplicates::list(&pool, page.limit, page.offset).await?;
    Ok(Json(DuplicatePage {
        items,
        limit: page.limit,
        offset: page.offset,
    }))
}

#[cfg(test)]
mod tests {
    //! Actual file probes, paginated reports, and observation invalidation.

    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    /// Copies share a digest without merging recordings; failures remove hits.
    #[tokio::test]
    async fn reports_exact_copies_and_invalidates_failed_observations() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let job = db::scans::create(&pool, &[], false).await.unwrap();
        let bytes = crate::library::metadata::tests::wave();
        for name in ["one.wav", "two.wav"] {
            let path = directory.path().join(name);
            std::fs::write(&path, &bytes).unwrap();
            db::indexing::store(
                &pool,
                job.id,
                crate::library::metadata::read(&path).unwrap().unwrap(),
            )
            .await
            .unwrap();
        }
        let copies = db::duplicates::list(&pool, 50, 0).await.unwrap();
        assert_eq!(copies.len(), 2);
        assert_eq!(copies[0].sha256, copies[1].sha256);
        assert_ne!(copies[0].recording_id, copies[1].recording_id);
        let service = crate::jobs::ScanService::new(pool.clone(), vec![]);
        let app = crate::api::router(
            &crate::config::DocsConfig::default(),
            &pool,
            service,
        );
        let app = crate::api::tests::with_admin(app, &pool).await;
        let response = app
            .clone()
            .oneshot(
                Request::get("/api/v1/library/duplicates?limit=1&offset=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = to_bytes(response.into_body(), 16384).await.unwrap();
        let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["file_id"], copies[1].file_id);
        let response = app
            .oneshot(
                Request::get("/api/v1/library/duplicates?limit=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        let mut changed = bytes.clone();
        let title = changed
            .windows(12)
            .position(|part| part == b"Source title")
            .unwrap();
        changed[title..title + 12].copy_from_slice(b"New tag name");
        let path = std::path::Path::new(&copies[1].path);
        std::fs::write(path, changed).unwrap();
        db::indexing::store(
            &pool,
            job.id,
            crate::library::metadata::read(path).unwrap().unwrap(),
        )
        .await
        .unwrap();
        assert!(db::duplicates::list(&pool, 50, 0).await.unwrap().is_empty());
        std::fs::write(path, &bytes).unwrap();
        db::indexing::store(
            &pool,
            job.id,
            crate::library::metadata::read(path).unwrap().unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(db::duplicates::list(&pool, 50, 0).await.unwrap().len(), 2);
        db::scans::record_error(&pool, job.id, &copies[0].path, "changed")
            .await
            .unwrap();
        assert!(db::duplicates::list(&pool, 50, 0).await.unwrap().is_empty());
        assert_eq!(std::fs::read(&copies[0].path).unwrap(), bytes);
        pool.close().await;
    }
}
