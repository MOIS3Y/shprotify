//! Administrator-only account provisioning and credential management.

use super::{
    error::{ApiError, ErrorResponse},
    pagination::Pagination,
};
use crate::{
    auth::{self, Admin, Role, User},
    db,
};
use axum::{
    Json,
    extract::{
        DefaultBodyLimit, OriginalUri, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use zeroize::Zeroizing;

/// Register administrative operations; every handler authenticates first.
pub(super) fn router(pool: SqlitePool) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_user, list_users))
        .routes(routes!(get_user))
        .routes(routes!(set_role))
        .routes(routes!(set_password))
        .routes(routes!(issue_key, revoke_keys))
        .routes(routes!(revoke_sessions))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(pool)
}

/// Provision a password account; normalize names using the CLI rules.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct CreateUser {
    /// Unique name: 1–64 ASCII letters, digits, '.', '_' or '-'.
    name: String,
    /// Exact 12–1024 UTF-8 bytes, without control characters.
    #[schema(format = Password)]
    password: String,
    /// Permissions; defaults to member, never inferred from account name.
    #[serde(default, deserialize_with = "required_role")]
    #[schema(default = "member")]
    role: Role,
}

/// Replace the target account role explicitly.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct SetRole {
    /// The current keys and sessions use this role on subsequent requests.
    #[serde(deserialize_with = "required_role")]
    role: Role,
}

/// Replace a password without exposing the old password or its hash.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct SetPassword {
    /// Exact 12–1024 UTF-8 bytes, without control characters.
    #[schema(format = Password)]
    password: String,
}

/// Bounded public identities, visible only to administrators.
#[derive(Serialize, ToSchema)]
struct UserPage {
    /// Account identities in ascending identifier order.
    items: Vec<User>,
    /// Maximum identities returned.
    limit: i64,
    /// Number of identities skipped.
    offset: i64,
}

/// One-time key delivery; the database stores only the credential digest.
#[derive(Serialize, ToSchema)]
struct IssuedKey {
    /// Account owning the new credential.
    user: User,
    /// Permanent Bearer key; copy now, as it cannot be retrieved again.
    token: String,
}

/// Create an account atomically with a password and no permanent keys.
/// Requires admin; the first administrator must be provisioned locally.
#[utoipa::path(post, path = "/users", tag = "Admin: Users",
    operation_id = "create_user", request_body = CreateUser,
    security(("personal_key" = [])), responses(
        (status = 201, body = User, description = "Account created",
            headers(("Location" = String, description = "Account URL"))),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 409, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 429, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn create_user(
    _admin: Admin,
    State(pool): State<SqlitePool>,
    OriginalUri(uri): OriginalUri,
    input: Result<Json<CreateUser>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = input?;
    let password = Zeroizing::new(input.password);
    let name = auth::name(&input.name).map_err(|_| {
        ApiError::Validation(
            "Name must be 1–64 ASCII letters, digits, '.', '_' or '-'",
        )
    })?;
    let hash = password_hash(password).await?;
    let user = db::users::create_password(&pool, &name, input.role, &hash)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|e| e.is_unique_violation())
            {
                ApiError::Conflict("Account name already exists")
            } else {
                error.into()
            }
        })?;
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, format!("{}/{}", uri.path(), user.id))],
        Json(user),
    )
        .into_response())
}

/// List account identities with bounded pagination; requires admin.
#[utoipa::path(get, path = "/users", tag = "Admin: Users",
    operation_id = "list_users", params(Pagination),
    security(("personal_key" = [])), responses(
        (status = 200, body = UserPage, description = "Account page"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn list_users(
    _admin: Admin,
    State(pool): State<SqlitePool>,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Result<Json<UserPage>, ApiError> {
    let Query(query) = query
        .map_err(|_| ApiError::BadRequest("Invalid pagination parameters"))?;
    query.validate()?;
    let items = db::users::list(&pool, query.limit, query.offset).await?;
    Ok(Json(UserPage {
        items,
        limit: query.limit,
        offset: query.offset,
    }))
}

/// Read an account identity without credentials; requires admin.
#[utoipa::path(get, path = "/users/{id}", tag = "Admin: Users",
    operation_id = "get_user",
    params(("id" = i64, Path, minimum = 1, description = "Account ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = User, description = "Account identity"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn get_user(
    _admin: Admin,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<User>, ApiError> {
    Ok(Json(
        db::users::find(&pool, account_id(id)?)
            .await?
            .ok_or(ApiError::NotFound)?,
    ))
}

/// Replace permissions; admin required even when changing one's own role.
/// Demoting the last admin is recoverable using the local CLI.
#[utoipa::path(put, path = "/users/{id}/role", tag = "Admin: Users",
    operation_id = "set_user_role", request_body = SetRole,
    params(("id" = i64, Path, minimum = 1, description = "Account ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = User, description = "Updated account"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn set_role(
    _admin: Admin,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
    input: Result<Json<SetRole>, JsonRejection>,
) -> Result<Json<User>, ApiError> {
    let id = account_id(id)?;
    let Json(input) = input?;
    Ok(Json(
        db::users::set_role_by_id(&pool, id, input.role)
            .await
            .map_err(account_error)?,
    ))
}

/// Reset a password and revoke all of its sessions, retaining personal keys.
/// Requires admin; returns no password, hash, or new session.
#[utoipa::path(put, path = "/users/{id}/password", tag = "Admin: Users",
    operation_id = "set_user_password", request_body = SetPassword,
    params(("id" = i64, Path, minimum = 1, description = "Account ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = User, description = "Password reset"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 429, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn set_password(
    _admin: Admin,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
    input: Result<Json<SetPassword>, JsonRejection>,
) -> Result<Json<User>, ApiError> {
    let id = account_id(id)?;
    let Json(input) = input?;
    let password = Zeroizing::new(input.password);
    db::users::find(&pool, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let hash = password_hash(password).await?;
    Ok(Json(
        db::sessions::set_password_by_id(&pool, id, &hash)
            .await
            .map_err(account_error)?,
    ))
}

/// Issue an additional permanent key; requires admin and returns it once.
#[utoipa::path(post, path = "/users/{id}/keys", tag = "Admin: Users",
    operation_id = "issue_user_key",
    params(("id" = i64, Path, minimum = 1, description = "Account ID")),
    security(("personal_key" = [])), responses(
        (status = 200, body = IssuedKey, description = "New personal key"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn issue_key(
    _admin: Admin,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Response, ApiError> {
    let id = account_id(id)?;
    let token = auth::token()?;
    let user = db::users::issue_by_id(&pool, id, &auth::digest(&token))
        .await
        .map_err(account_error)?;
    Ok((
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(IssuedKey { user, token }),
    )
        .into_response())
}

/// Revoke target keys, retaining sessions; requires admin.
#[utoipa::path(delete, path = "/users/{id}/keys", tag = "Admin: Users",
    operation_id = "revoke_user_keys",
    params(("id" = i64, Path, minimum = 1, description = "Account ID")),
    security(("personal_key" = [])), responses(
        (status = 204, description = "Personal keys revoked"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn revoke_keys(
    _admin: Admin,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    db::users::revoke_by_id(&pool, account_id(id)?)
        .await
        .map_err(account_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Revoke target sessions, retaining password login; requires admin.
#[utoipa::path(delete, path = "/users/{id}/sessions", tag = "Admin: Users",
    operation_id = "revoke_user_sessions",
    params(("id" = i64, Path, minimum = 1, description = "Account ID")),
    security(("personal_key" = [])), responses(
        (status = 204, description = "Password sessions revoked"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 403, response = ErrorResponse),
        (status = 404, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn revoke_sessions(
    _admin: Admin,
    State(pool): State<SqlitePool>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    db::sessions::revoke_all(&pool, account_id(id)?)
        .await
        .map_err(account_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Parse roles from strings so invalid types remain field validation errors.
fn required_role<'de, D>(deserializer: D) -> Result<Role, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Role::deserialize(serde::de::value::StringDeserializer::new(value))
}

/// Reject malformed and nonpositive identifiers before any account query.
fn account_id(id: Result<Path<i64>, PathRejection>) -> Result<i64, ApiError> {
    let Path(id) =
        id.map_err(|_| ApiError::BadRequest("Invalid account ID"))?;
    if id <= 0 {
        return Err(ApiError::BadRequest("Account ID must be positive"));
    }
    Ok(id)
}

/// Report missing accounts as 404, retaining unexpected database diagnostics.
fn account_error(error: sqlx::Error) -> ApiError {
    match error {
        sqlx::Error::RowNotFound => ApiError::NotFound,
        other => other.into(),
    }
}

/// Validate and hash with the same bounded worker pool used by login.
async fn password_hash(
    password: Zeroizing<String>,
) -> Result<String, ApiError> {
    auth::password::validate(&password).map_err(|_| {
        ApiError::Validation(
            "Password must be 12–1024 UTF-8 bytes without control characters",
        )
    })?;
    let permit = auth::password::admit()?;
    Ok(auth::password::hash_admitted(password, permit).await?)
}

#[cfg(test)]
mod tests {
    //! Administrative contracts use real keys, passwords and migrated SQLite.

    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{HeaderMap, Request},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Send a request through the full production router, without sockets.
    async fn request(
        app: &Router,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some(body) => {
                builder =
                    builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, headers, body)
    }

    /// Isolated operator/member fixtures without any authorization bypass.
    struct Fixture {
        /// Keep the file-backed database directory alive until teardown.
        _directory: tempfile::TempDir,
        /// Shared account storage.
        pool: SqlitePool,
        /// Production API, with Swagger enabled for schema checks.
        app: Router,
        /// Local operator's personal key.
        admin: String,
        /// Ordinary member's personal key.
        member: String,
    }

    impl Fixture {
        /// Provision the initial administrator through local storage helpers.
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let pool = db::connect(&directory.path().join("catalog.db"))
                .await
                .unwrap();
            let admin = auth::token().unwrap();
            let member = auth::token().unwrap();
            db::users::create(&pool, "operator", &auth::digest(&admin))
                .await
                .unwrap();
            db::users::set_role(&pool, "operator", Role::Admin)
                .await
                .unwrap();
            db::users::create(&pool, "member", &auth::digest(&member))
                .await
                .unwrap();
            let app = crate::api::router(
                &crate::config::AuthConfig::default(),
                &crate::config::DocsConfig { enabled: true },
                &pool,
                crate::jobs::ScanService::new(pool.clone(), vec![]),
            );
            Self {
                _directory: directory,
                pool,
                app,
                admin,
                member,
            }
        }

        /// Send an authenticated administrative request.
        async fn admin(
            &self,
            method: &str,
            path: &str,
            body: Option<Value>,
        ) -> (StatusCode, HeaderMap, Value) {
            request(&self.app, method, path, Some(&self.admin), body).await
        }
    }

    /// Every account route, including HEAD, rejects anonymous and member use.
    #[tokio::test]
    async fn account_operations_authenticate_before_validating_or_writing() {
        let fixture = Fixture::new().await;
        for (token, status) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(fixture.member.as_str()), StatusCode::FORBIDDEN),
        ] {
            for (method, path) in [
                ("GET", "/api/v1/users?limit=invalid"),
                ("HEAD", "/api/v1/users"),
                ("GET", "/api/v1/users/invalid"),
                ("HEAD", "/api/v1/users/1"),
                ("POST", "/api/v1/users"),
                ("PUT", "/api/v1/users/invalid/role"),
                ("PUT", "/api/v1/users/invalid/password"),
                ("POST", "/api/v1/users/invalid/keys"),
                ("DELETE", "/api/v1/users/invalid/keys"),
                ("DELETE", "/api/v1/users/invalid/sessions"),
            ] {
                let (actual, headers, _) = request(
                    &fixture.app,
                    method,
                    path,
                    token,
                    Some(json!({})),
                )
                .await;
                assert_eq!(actual, status, "{method} {path}");
                assert_eq!(
                    headers.contains_key(header::WWW_AUTHENTICATE),
                    status == StatusCode::UNAUTHORIZED
                );
            }
        }
        let counts: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM users), \
            (SELECT COUNT(*) FROM user_keys), \
            (SELECT COUNT(*) FROM user_passwords), \
            (SELECT COUNT(*) FROM user_sessions)",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(counts, (2, 2, 0, 0));
        fixture.pool.close().await;
    }

    /// Administrative input uses bounded pages and safe, consistent errors.
    #[tokio::test]
    async fn validates_account_payloads_and_missing_targets() {
        let fixture = Fixture::new().await;
        for query in ["limit=0", "limit=101", "offset=-1", "role=admin"] {
            assert_eq!(
                fixture
                    .admin("GET", &format!("/api/v1/users?{query}"), None)
                    .await
                    .0,
                StatusCode::BAD_REQUEST
            );
        }
        for (method, suffix, body) in [
            ("GET", "", None),
            ("PUT", "/role", Some(json!({"role":"member"}))),
            (
                "PUT",
                "/password",
                Some(json!({"password":"long password"})),
            ),
            ("POST", "/keys", None),
            ("DELETE", "/keys", None),
            ("DELETE", "/sessions", None),
        ] {
            for (id, status) in [
                ("invalid", StatusCode::BAD_REQUEST),
                ("0", StatusCode::BAD_REQUEST),
                ("-1", StatusCode::BAD_REQUEST),
                ("99999", StatusCode::NOT_FOUND),
            ] {
                let (actual, _, value) = fixture
                    .admin(
                        method,
                        &format!("/api/v1/users/{id}{suffix}"),
                        body.clone(),
                    )
                    .await;
                assert_eq!(actual, status);
                assert!(value["error"]["code"].is_string());
            }
        }
        for (index, body) in [
            json!({}),
            json!({"name":"alice", "password":null}),
            json!({"name":"../", "password":"long password"}),
            json!({"name":"alice", "password":"short"}),
            json!({"name":"alice", "password":"long password", "role":null}),
            json!({"name":"alice", "password":"long password", "role":"root"}),
            json!({"name":"alice", "password":"long password", "id":999}),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                fixture.admin("POST", "/api/v1/users", Some(body)).await.0,
                StatusCode::UNPROCESSABLE_ENTITY,
                "payload {index}"
            );
        }
        assert_eq!(
            fixture.admin("POST", "/api/v1/users", None).await.0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        assert_eq!(
            fixture
                .admin(
                    "POST",
                    "/api/v1/users",
                    Some(json!({
                        "name":"alice", "password":"x".repeat(5000)
                    }))
                )
                .await
                .0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        for body in [
            json!({}),
            json!({"role":null}),
            json!({"role":false}),
            json!({"role":42}),
            json!({"role":{}}),
            json!({"role":"root"}),
            json!({"role":"admin", "user_id":2}),
        ] {
            assert_eq!(
                fixture
                    .admin("PUT", "/api/v1/users/2/role", Some(body))
                    .await
                    .0,
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
        assert_eq!(
            fixture
                .admin(
                    "PUT",
                    "/api/v1/users/2/password",
                    Some(json!({"password":"short"}))
                )
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let (_, _, page) = fixture
            .admin("GET", "/api/v1/users?limit=1&offset=1", None)
            .await;
        assert_eq!(
            page["items"],
            json!([{
                "id":2, "name":"member", "role":"member"
            }])
        );
        assert_eq!(page["limit"], 1);
        assert_eq!(page["offset"], 1);
        assert_eq!(
            fixture
                .admin("GET", "/api/v1/users?offset=999", None)
                .await
                .2["items"],
            json!([])
        );
        fixture.pool.close().await;
    }

    /// Account roles and credential types remain independently manageable.
    #[tokio::test]
    async fn manages_accounts_and_credentials_without_losing_personal_data() {
        let _guard = auth::password::test_guard().await;
        let fixture = Fixture::new().await;
        let (status, headers, user) = fixture
            .admin(
                "POST",
                "/api/v1/users",
                Some(
                    json!({"name":" Alice ", "password":"a strong password"}),
                ),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(user, json!({"id":3,"name":"alice","role":"member"}));
        assert_eq!(headers[header::LOCATION], "/api/v1/users/3");
        let keys: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_keys WHERE user_id = 3",
        )
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(keys, 0);
        assert_eq!(fixture.admin("POST", "/api/v1/users", Some(json!({
            "name":"ALICE", "password":"another password", "role":"admin"
        }))).await.0, StatusCode::CONFLICT);
        let (_, _, session) = request(
            &fixture.app,
            "POST",
            "/api/v1/auth/login",
            None,
            Some(json!({"name":"alice","password":"a strong password"})),
        )
        .await;
        let session = session["access_token"].as_str().unwrap();
        let (status, headers, issued) =
            fixture.admin("POST", "/api/v1/users/3/keys", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        let key = issued["token"].as_str().unwrap();
        for token in [key, session] {
            assert_eq!(
                request(&fixture.app, "GET", "/api/v1/me", Some(token), None)
                    .await
                    .2["name"],
                "alice"
            );
        }
        sqlx::query("INSERT INTO recordings (title) VALUES ('Favorite')")
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert_eq!(
            request(
                &fixture.app,
                "PUT",
                "/api/v1/me/recordings/1/reaction",
                Some(key),
                Some(json!({"reaction":"like"}))
            )
            .await
            .0,
            StatusCode::OK
        );
        let updated = fixture
            .admin(
                "PUT",
                "/api/v1/users/3/role",
                Some(json!({"role":"admin"})),
            )
            .await;
        assert_eq!(updated.2["role"], "admin");
        for token in [key, session] {
            assert_eq!(
                request(
                    &fixture.app,
                    "GET",
                    "/api/v1/users",
                    Some(token),
                    None
                )
                .await
                .0,
                StatusCode::OK
            );
        }
        fixture
            .admin(
                "PUT",
                "/api/v1/users/3/role",
                Some(json!({"role":"member"})),
            )
            .await;
        for token in [key, session] {
            assert_eq!(
                request(
                    &fixture.app,
                    "GET",
                    "/api/v1/users",
                    Some(token),
                    None
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
        }
        let first = auth::password::admit().unwrap();
        let second = auth::password::admit().unwrap();
        let busy = fixture
            .admin(
                "PUT",
                "/api/v1/users/3/password",
                Some(json!({"password":"another password"})),
            )
            .await;
        assert_eq!(busy.0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(busy.1[header::RETRY_AFTER], "1");
        drop((first, second));
        let reset = fixture
            .admin(
                "PUT",
                "/api/v1/users/3/password",
                Some(json!({"password":"another password"})),
            )
            .await;
        assert_eq!(reset.2, user);
        assert_eq!(
            request(&fixture.app, "GET", "/api/v1/me", Some(session), None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &fixture.app,
                "POST",
                "/api/v1/auth/login",
                None,
                Some(json!({"name":"alice","password":"a strong password"}))
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
        let mut sessions = Vec::new();
        for _ in 0..2 {
            let (status, _, session) = request(
                &fixture.app,
                "POST",
                "/api/v1/auth/login",
                None,
                Some(json!({
                    "name":"alice","password":"another password"
                })),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            sessions
                .push(session["access_token"].as_str().unwrap().to_owned());
        }
        assert_eq!(
            fixture
                .admin("DELETE", "/api/v1/users/3/keys", None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(&fixture.app, "GET", "/api/v1/me", Some(key), None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        for token in &sessions {
            assert_eq!(
                request(&fixture.app, "GET", "/api/v1/me", Some(token), None)
                    .await
                    .0,
                StatusCode::OK
            );
        }
        let (_, _, issued) =
            fixture.admin("POST", "/api/v1/users/3/keys", None).await;
        let new_key = issued["token"].as_str().unwrap();
        assert_ne!(new_key, key);
        for _ in 0..2 {
            assert_eq!(
                fixture
                    .admin("DELETE", "/api/v1/users/3/sessions", None)
                    .await
                    .0,
                StatusCode::NO_CONTENT
            );
        }
        for token in &sessions {
            assert_eq!(
                request(&fixture.app, "GET", "/api/v1/me", Some(token), None)
                    .await
                    .0,
                StatusCode::UNAUTHORIZED
            );
        }
        let favorites = request(
            &fixture.app,
            "GET",
            "/api/v1/me/favorites",
            Some(new_key),
            None,
        )
        .await;
        assert_eq!(favorites.2["items"][0]["recording_id"], 1);
        let (_, _, page) = fixture.admin("GET", "/api/v1/users", None).await;
        assert_eq!(page["items"].as_array().unwrap().len(), 3);
        assert_eq!(
            fixture.admin("GET", "/api/v1/users/3", None).await.2,
            user
        );
        assert_eq!(fixture.admin("POST", "/api/v1/users", Some(json!({
            "name":"other-admin", "password":"long password", "role":"admin"
        }))).await.2["role"], "admin");
        let (_, _, operator) = fixture
            .admin(
                "PUT",
                "/api/v1/users/1/role",
                Some(json!({"role":"member"})),
            )
            .await;
        assert_eq!(operator["role"], "member");
        assert_eq!(
            fixture.admin("GET", "/api/v1/users", None).await.0,
            StatusCode::FORBIDDEN
        );
        db::users::set_role(&fixture.pool, "operator", Role::Admin)
            .await
            .unwrap();
        assert_eq!(
            fixture
                .admin("DELETE", "/api/v1/users/1/keys", None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            fixture.admin("GET", "/api/v1/users", None).await.0,
            StatusCode::UNAUTHORIZED
        );
        fixture.pool.close().await;
    }
}
