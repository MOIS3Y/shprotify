//! Administrator-only unregistering and managed-file removal jobs.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    auth::{Admin, CurrentUser},
    db,
    jobs::removals::RemovalService,
};
use axum::{
    Json,
    extract::{
        FromRef, Path, Query, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes share the application-owned executor and its shutdown lifecycle.
pub(super) fn router(
    pool: SqlitePool,
    service: RemovalService,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(remove_file))
        .routes(routes!(get_removal))
        .routes(routes!(list_removals))
        .with_state(RemovalState { pool, service })
}

/// HTTP state, separate from the worker's task ownership.
#[derive(Clone)]
struct RemovalState {
    /// Durable history and authorization source.
    pool: SqlitePool,
    /// Bounded executor supervised by application startup.
    service: RemovalService,
}
impl FromRef<RemovalState> for SqlitePool {
    /// Reuse the common authentication extractor.
    fn from_ref(state: &RemovalState) -> Self {
        state.pool.clone()
    }
}

/// Explicit opt-in to deleting verified server-owned bytes.
#[derive(Default, Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
#[into_params(parameter_in=Query)]
struct RemoveQuery {
    /// False only unregisters; external collection bytes are always preserved.
    remove_file: bool,
}

/// Accept a durable operation; recording and all personal references survive.
/// Physical removal is limited to verified managed acquisition objects.
/// Unregistered external files may be rediscovered by subsequent scans.
#[utoipa::path(delete,path="/recordings/{id}/files/{file_id}",
    tag="Admin: Library",security(("personal_key"=[])),
    params(("id"=i64,Path,minimum=1),("file_id"=i64,Path,minimum=1),
        RemoveQuery), responses(
        (status=202,body=db::removals::Removal,description="Removal accepted",
            headers(("Location"=String,description="Operation URL"))),
        (status=400,response=ErrorResponse),
        (status=401,response=ErrorResponse),
        (status=403,response=ErrorResponse),
        (status=404,response=ErrorResponse),
        (status=409,response=ErrorResponse),
        (status=500,response=ErrorResponse)
    ))]
async fn remove_file(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<RemovalState>,
    ids: Result<Path<(i64, i64)>, PathRejection>,
    query: Result<Query<RemoveQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Path((recording, file)) =
        ids.map_err(|_| ApiError::BadRequest("Invalid recording or file ID"))?;
    if recording <= 0 || file <= 0 {
        return Err(ApiError::BadRequest("Identifiers must be positive"));
    }
    let Query(query) = query
        .map_err(|_| ApiError::BadRequest("Invalid removal parameters"))?;
    let id = state
        .service
        .start(recording, file, user.id, query.remove_file)
        .await?;
    let operation = db::removals::find(&state.pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok((
        StatusCode::ACCEPTED,
        [(
            header::LOCATION,
            format!("/api/v1/library/file-removals/{id}"),
        )],
        Json(operation),
    )
        .into_response())
}

/// Read the outcome of a shared administrative operation.
#[utoipa::path(get,path="/library/file-removals/{id}",
    tag="Admin: Library",security(("personal_key"=[])),
    params(("id"=i64,Path,minimum=1)),responses(
        (status=200,body=db::removals::Removal,description="Removal status"),
        (status=400,response=ErrorResponse),
        (status=401,response=ErrorResponse),
        (status=403,response=ErrorResponse),
        (status=404,response=ErrorResponse),
        (status=500,response=ErrorResponse)
    ))]
async fn get_removal(
    _admin: Admin,
    State(state): State<RemovalState>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<db::removals::Removal>, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid removal ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("ID must be positive"));
    }
    Ok(Json(
        db::removals::find(&state.pool, id)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// Newest-first operation history with ordinary bounded pagination.
#[derive(Serialize, ToSchema)]
struct RemovalPage {
    /// Accepted shared operations, including completed and failed jobs.
    items: Vec<db::removals::Removal>,
    /// Requested page size.
    limit: i64,
    /// Number of skipped operations.
    offset: i64,
}

/// Inspect recent operations without exposing filesystem paths or hashes.
#[utoipa::path(get,path="/library/file-removals",tag="Admin: Library",
    security(("personal_key"=[])),params(Pagination),responses(
        (status=200,body=RemovalPage,description="Removal history"),
        (status=400,response=ErrorResponse),
        (status=401,response=ErrorResponse),
        (status=403,response=ErrorResponse),
        (status=500,response=ErrorResponse)
    ))]
async fn list_removals(
    _admin: Admin,
    State(state): State<RemovalState>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<RemovalPage>, ApiError> {
    let Query(page) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    page.validate()?;
    Ok(Json(RemovalPage {
        items: db::removals::list(&state.pool, page.limit, page.offset)
            .await?,
        limit: page.limit,
        offset: page.offset,
    }))
}
