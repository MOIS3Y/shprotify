//! Interval settings; private feed owners and administrators retain control.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    auth::{Admin, CurrentUser},
    automation::{AutomationError, Schedule, ScheduleUpdate, Target},
    config::AutomationConfig,
    db,
    downloads::policy::ImportPolicy,
    library::enrichment::EntityKind,
};
use axum::{
    Json,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Immutable admission policy and normal application authentication storage.
#[derive(Clone)]
struct AutomationState {
    /// Same SQLite pool as feeds, metadata and sessions.
    pool: SqlitePool,
    /// Operator interval bounds; changing these requires a restart.
    config: AutomationConfig,
    /// Managed import choices; no tool paths or secrets are exposed.
    import_policy: ImportPolicy,
}

impl axum::extract::FromRef<AutomationState> for SqlitePool {
    /// Authenticate before parsing IDs, queries or JSON settings.
    fn from_ref(state: &AutomationState) -> Self {
        state.pool.clone()
    }
}

/// All operations remain registered while automatic execution is disabled.
pub(super) fn router(
    pool: SqlitePool,
    config: AutomationConfig,
    import_policy: ImportPolicy,
) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(status))
        .routes(routes!(metadata_targets))
        .routes(routes!(get_feed, set_feed))
        .routes(routes!(get_metadata, set_metadata))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(AutomationState {
            pool,
            config,
            import_policy,
        })
}

/// Administration view without exposing integration keys or filesystem paths.
#[derive(Serialize, ToSchema)]
struct AutomationStatus {
    /// Read-only startup policy; no cron expressions or external service
    /// needed.
    config: AutomationConfig,
    /// Singleton incremental scan schedule controlled through TOML.
    library: Schedule,
    /// Read-only finite import policy configured through TOML.
    import_policy: ImportPolicy,
}

/// One explicitly selected catalog metadata target and its execution status.
#[derive(Serialize, ToSchema)]
struct MetadataSchedule {
    /// Artist/release/recording namespace.
    kind: EntityKind,
    /// Positive shared catalog ID.
    id: i64,
    /// Versioned opt-in and latest automatic proposal job.
    schedule: Schedule,
}

/// Bounded administrator view of configured metadata targets.
#[derive(Serialize, ToSchema)]
struct MetadataSchedulePage {
    /// Includes disabled schedules for later re-enabling.
    items: Vec<MetadataSchedule>,
    /// Effective page size.
    limit: i64,
    /// Number of configured targets skipped.
    offset: i64,
}

impl From<AutomationError> for ApiError {
    /// Never expose SQL or another account's schedule existence.
    fn from(error: AutomationError) -> Self {
        match error {
            AutomationError::NotFound => Self::NotFound,
            AutomationError::Conflict(message) => Self::Conflict(message),
            AutomationError::Invalid(message) => Self::Validation(message),
            AutomationError::Storage(error) => error.into(),
        }
    }
}

/// Schedule timestamps and personal opt-ins must not enter shared caches.
fn private(value: impl IntoResponse) -> Response {
    let mut response = value.into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

/// Accept positive route IDs after the principal has been authenticated.
fn feed_id(path: Result<Path<i64>, PathRejection>) -> Result<i64, ApiError> {
    let Path(id) =
        path.map_err(|_| ApiError::BadRequest("Invalid feed ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("ID must be positive"));
    }
    Ok(id)
}

/// Strict catalog namespace parsing prevents arbitrary scheduler target types.
fn entity(
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
) -> Result<(EntityKind, i64), ApiError> {
    let Path((kind, id)) =
        path.map_err(|_| ApiError::BadRequest("Invalid metadata kind or ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("ID must be positive"));
    }
    Ok((kind, id))
}

/// Read global interval/import policy and incremental scan status as admin.
#[utoipa::path(get, path="/automation", tag="automation",
    operation_id="get_automation_status", security(("personal_key"=[])),
    responses((status=200, description="Configuration and scan state",
        body=AutomationStatus), (status=401, response=ErrorResponse),
        (status=403, response=ErrorResponse),
        (status=500, response=ErrorResponse)))]
async fn status(
    _admin: Admin,
    State(state): State<AutomationState>,
) -> Result<Response, ApiError> {
    let library =
        db::automation::get(&state.pool, &state.config, 0, Target::Library)
            .await?;
    Ok(private(Json(AutomationStatus {
        config: state.config,
        library,
        import_policy: state.import_policy,
    })))
}

/// Review administrator-selected metadata targets; no implicit whole-catalog
/// run.
#[utoipa::path(get, path="/automation/metadata", tag="automation",
    operation_id="list_metadata_schedules", security(("personal_key"=[])),
    params(Pagination),
    responses((status=200, description="Configured targets",
        body=MetadataSchedulePage), (status=400, response=ErrorResponse),
        (status=401, response=ErrorResponse),
        (status=403, response=ErrorResponse),
        (status=500, response=ErrorResponse)))]
async fn metadata_targets(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<AutomationState>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(page) =
        query.map_err(|_| ApiError::BadRequest("Invalid pagination"))?;
    page.validate()?;
    let mut items = Vec::new();
    for (kind, id) in
        db::automation::metadata_targets(&state.pool, page.limit, page.offset)
            .await?
    {
        let kind = match kind.as_str() {
            "artist" => EntityKind::Artist,
            "release" => EntityKind::Release,
            "recording" => EntityKind::Recording,
            _ => {
                return Err(ApiError::Internal(anyhow::anyhow!(
                    "Invalid persisted schedule namespace"
                )));
            }
        };
        let schedule = db::automation::get(
            &state.pool,
            &state.config,
            user.id,
            Target::Metadata(kind, id),
        )
        .await?;
        items.push(MetadataSchedule { kind, id, schedule });
    }
    Ok(private(Json(MetadataSchedulePage {
        items,
        limit: page.limit,
        offset: page.offset,
    })))
}

/// Read interval settings; GET does not create a schedule.
#[utoipa::path(get, path="/me/discovery/feeds/{id}/schedule", tag="automation",
    operation_id="get_feed_schedule", security(("personal_key"=[])),
    params(("id"=i64, Path, minimum=1)),
    responses((status=200, description="Schedule settings and outcome",
        body=Schedule), (status=400, response=ErrorResponse),
        (status=401, response=ErrorResponse),
        (status=404, response=ErrorResponse),
        (status=500, response=ErrorResponse)))]
async fn get_feed(
    CurrentUser(user): CurrentUser,
    State(state): State<AutomationState>,
    path: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let target = Target::Discovery(feed_id(path)?);
    let schedule =
        db::automation::get(&state.pool, &state.config, user.id, target)
            .await?;
    Ok(private(Json(schedule)))
}

/// Replace settings with a version check; disabling leaves active work
/// attached.
#[utoipa::path(put, path="/me/discovery/feeds/{id}/schedule", tag="automation",
    operation_id="set_feed_schedule", security(("personal_key"=[])),
    request_body=ScheduleUpdate,
    params(("id"=i64, Path, minimum=1)),
    responses((status=200, description="Schedule settings and outcome",
        body=Schedule), (status=400, response=ErrorResponse),
        (status=401, response=ErrorResponse),
        (status=404, response=ErrorResponse),
        (status=409, response=ErrorResponse),
        (status=413, response=ErrorResponse),
        (status=415, response=ErrorResponse),
        (status=422, response=ErrorResponse),
        (status=500, response=ErrorResponse)))]
async fn set_feed(
    CurrentUser(user): CurrentUser,
    State(state): State<AutomationState>,
    path: Result<Path<i64>, PathRejection>,
    body: Result<Json<ScheduleUpdate>, JsonRejection>,
) -> Result<Response, ApiError> {
    let target = Target::Discovery(feed_id(path)?);
    let Json(input) = body?;
    let schedule = db::automation::set(
        &state.pool,
        &state.config,
        user.id,
        target,
        input,
    )
    .await?;
    Ok(private(Json(schedule)))
}

/// Read interval settings; GET does not create a schedule. Metadata jobs only
/// produce proposals for manual review.
#[utoipa::path(get, path="/metadata/{kind}/{id}/schedule", tag="automation",
    operation_id="get_metadata_schedule", security(("personal_key"=[])),
    params(("id"=i64, Path, minimum=1), ("kind"=EntityKind, Path)),
    responses((status=200, description="Schedule settings and outcome",
        body=Schedule), (status=400, response=ErrorResponse),
        (status=401, response=ErrorResponse),
        (status=403, response=ErrorResponse),
        (status=404, response=ErrorResponse),
        (status=500, response=ErrorResponse)))]
async fn get_metadata(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<AutomationState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    let target = Target::Metadata(kind, id);
    let schedule =
        db::automation::get(&state.pool, &state.config, user.id, target)
            .await?;
    Ok(private(Json(schedule)))
}

/// Replace settings with a version check; disabling leaves active work
/// attached. Metadata jobs only produce proposals for manual review.
#[utoipa::path(put, path="/metadata/{kind}/{id}/schedule", tag="automation",
    operation_id="set_metadata_schedule", security(("personal_key"=[])),
    request_body=ScheduleUpdate,
    params(("id"=i64, Path, minimum=1), ("kind"=EntityKind, Path)),
    responses((status=200, description="Schedule settings and outcome",
        body=Schedule), (status=400, response=ErrorResponse),
        (status=401, response=ErrorResponse),
        (status=403, response=ErrorResponse),
        (status=404, response=ErrorResponse),
        (status=409, response=ErrorResponse),
        (status=413, response=ErrorResponse),
        (status=415, response=ErrorResponse),
        (status=422, response=ErrorResponse),
        (status=500, response=ErrorResponse)))]
async fn set_metadata(
    _admin: Admin,
    CurrentUser(user): CurrentUser,
    State(state): State<AutomationState>,
    path: Result<Path<(EntityKind, i64)>, PathRejection>,
    body: Result<Json<ScheduleUpdate>, JsonRejection>,
) -> Result<Response, ApiError> {
    let (kind, id) = entity(path)?;
    let target = Target::Metadata(kind, id);
    let Json(input) = body?;
    let schedule = db::automation::set(
        &state.pool,
        &state.config,
        user.id,
        target,
        input,
    )
    .await?;
    Ok(private(Json(schedule)))
}

#[cfg(test)]
mod tests {
    //! Real member/admin credentials, strict settings and documented
    //! contracts.
    use super::*;
    use crate::db::automation::tests::{config, feed};
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Full production router, with scheduling dispatch kept explicitly idle.
    async fn app(pool: &SqlitePool) -> (Router, String, String, i64) {
        let alice = crate::auth::token().unwrap();
        let bob = crate::auth::token().unwrap();
        db::users::issue_by_id(pool, 1, &crate::auth::digest(&alice))
            .await
            .unwrap();
        db::users::issue_by_id(pool, 2, &crate::auth::digest(&bob))
            .await
            .unwrap();
        sqlx::query("UPDATE users SET role='admin' WHERE id=2")
            .execute(pool)
            .await
            .unwrap();
        let id =
            feed(pool, 1, crate::discovery::FeedRequest::Personal {}).await;
        let scans = crate::jobs::ScanService::new(pool.clone(), vec![]);
        let metadata = crate::jobs::metadata::MetadataService::new(
            pool.clone(),
            scans.roots(),
            crate::config::MetadataConfig::default(),
        )
        .unwrap();
        let discovery = crate::jobs::discovery::DiscoveryService::new(
            pool.clone(),
            crate::config::DiscoveryConfig::default(),
        )
        .unwrap();
        let downloads = crate::jobs::downloads::DownloadService::new(
            pool.clone(),
            crate::config::DownloadsConfig::default(),
            None,
        );
        let app = crate::api::router_with_services(
            &crate::config::AuthConfig::default(),
            &crate::config::DocsConfig { enabled: true },
            pool,
            crate::api::Services {
                scans,
                metadata,
                discovery,
                downloads,
                automation: config(),
            },
        );
        (app, alice, bob, id)
    }

    /// Send strict JSON and inspect safe response envelopes through Axum.
    async fn request(
        app: &Router,
        method: &str,
        path: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if !token.is_empty() {
            builder = builder
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = if let Some(body) = body {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        } else {
            Body::empty()
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        if status.is_success() && path != "/openapi.json" {
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "private, no-store"
            );
        }
        let bytes = to_bytes(response.into_body(), 512 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// Only a feed owner changes its interval; admin metadata settings are
    /// separate.
    #[tokio::test]
    async fn manages_private_feeds_and_selected_admin_metadata_schedules() {
        let (_dir, pool) = db::discovery::tests::setup().await;
        let (app, alice, bob, id) = app(&pool).await;
        let path = format!("/api/v1/me/discovery/feeds/{id}/schedule");
        let (status, initial) =
            request(&app, "GET", &path, &alice, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(initial["version"], 0);
        assert_eq!(initial["enabled"], false);
        assert_eq!(
            request(&app, "GET", &path, &bob, None).await.0,
            StatusCode::NOT_FOUND
        );
        let body = json!({"version":0,"enabled":true,"interval_seconds":60});
        let (status, schedule) =
            request(&app, "PUT", &path, &alice, Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{schedule}");
        assert_eq!(schedule["version"], 1);
        assert_eq!(schedule["effective_interval_seconds"], 60);
        assert_eq!(schedule["min_interval_seconds"], 60);
        assert_eq!(schedule["max_interval_seconds"], 2592000);
        assert_eq!(
            request(&app, "PUT", &path, &alice, Some(body)).await.0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(&app, "GET", "/api/v1/automation", &alice, None)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let (status, global) =
            request(&app, "GET", "/api/v1/automation", &bob, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(global["import_policy"]["duplicates"], "reuse");
        assert!(global["config"].get("lastfm_api_key").is_none());
        let path = "/api/v1/metadata/recording/1/schedule";
        let body =
            json!({"version":0,"enabled":true,"interval_seconds":86400});
        assert_eq!(
            request(&app, "PUT", path, &alice, Some(body.clone()))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let (status, schedule) =
            request(&app, "PUT", path, &bob, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{schedule}");
        assert!(schedule["paused_reason"].is_string());
        let (status, page) =
            request(&app, "GET", "/api/v1/automation/metadata", &bob, None)
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["items"][0]["kind"], "recording");
        assert_eq!(page["items"][0]["id"], 1);
        pool.close().await;
    }

    /// Authentication precedes parsing and all schedule operations enter
    /// OpenAPI.
    #[tokio::test]
    async fn validates_settings_and_keeps_disabled_execution_configurable() {
        let (_dir, pool) = db::discovery::tests::setup().await;
        let (app, alice, bob, id) = app(&pool).await;
        let path = format!("/api/v1/me/discovery/feeds/{id}/schedule");
        assert_eq!(
            request(&app, "PUT", &path, "", Some(json!({}))).await.0,
            StatusCode::UNAUTHORIZED
        );
        for body in [
            json!({"version":0,"enabled":true,"interval_seconds":59}),
            json!({"version":-1,"enabled":true,"interval_seconds":60}),
            json!({"version":0,"enabled":true,"interval_seconds":60,
                "cron":"*"}),
            json!({"version":0,"enabled":true,"interval_seconds":2592001}),
        ] {
            assert_eq!(
                request(&app, "PUT", &path, &alice, Some(body)).await.0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        assert_eq!(
            request(
                &app,
                "GET",
                "/api/v1/automation/metadata?limit=101",
                &bob,
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &app,
                "GET",
                "/api/v1/metadata/unknown/1/schedule",
                &bob,
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let (_, schema) =
            request(&app, "GET", "/openapi.json", "", None).await;
        let mut count = 0;
        for methods in schema["paths"].as_object().unwrap().values() {
            for operation in methods.as_object().unwrap().values() {
                if operation["tags"][0] == "automation" {
                    count += 1;
                    assert_eq!(
                        operation["security"][0]["personal_key"],
                        json!([])
                    );
                    assert!(operation["responses"]["401"].is_object());
                }
            }
        }
        assert_eq!(count, 6);
        let config = AutomationConfig::default();
        let saved = db::automation::set(
            &pool,
            &config,
            1,
            Target::Discovery(id),
            ScheduleUpdate {
                version: 0,
                enabled: true,
                interval_seconds: 3600,
            },
        )
        .await
        .unwrap();
        assert!(saved.enabled);
        assert!(saved.paused_reason.is_some());
        assert!(
            db::automation::claim(&pool, &config, 1)
                .await
                .unwrap()
                .is_none()
        );
        pool.close().await;
    }
}
