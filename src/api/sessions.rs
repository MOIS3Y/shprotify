//! Password login and logout; sessions share Bearer transport with API keys.

use super::error::{ApiError, ErrorResponse};
use crate::{auth, config::AuthConfig, db};
use axum::{
    Json,
    extract::{DefaultBodyLimit, FromRef, State, rejection::JsonRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use zeroize::Zeroizing;

/// Register bounded JSON login and session-only logout operations.
pub(super) fn router(pool: SqlitePool, auth: AuthConfig) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(login))
        .layer(DefaultBodyLimit::max(4096))
        .routes(routes!(logout))
        .with_state(SessionState { pool, auth })
}

/// Immutable startup policy and database for session operations.
#[derive(Clone)]
struct SessionState {
    /// Credentials and issued session records.
    pool: SqlitePool,
    /// Policy applies only to new sessions; stored expiry remains unchanged.
    auth: AuthConfig,
}

impl FromRef<SessionState> for SqlitePool {
    /// Let authentication guards reuse the session database pool.
    fn from_ref(state: &SessionState) -> Self {
        state.pool.clone()
    }
}

/// Password credentials; deliberately has no Debug implementation.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct Login {
    /// Account name, normalized in the same way as the local CLI.
    name: String,
    /// 12–1024 UTF-8 bytes; no trimming or Unicode normalization.
    #[schema(format = Password)]
    password: String,
}

/// A one-time delivery of a session credential with an absolute expiry.
#[derive(Serialize, ToSchema)]
struct Session {
    /// Raw credential; persisted only as a SHA-256 digest.
    access_token: String,
    /// Always Bearer; supply the token in the Authorization header.
    token_type: &'static str,
    /// Absolute expiration time in Unix seconds, unaffected by activity.
    expires_at: i64,
}

/// Issue a session with the configured lifetime (default: 30 days).
/// Unknown users, key-only accounts and wrong passwords all return 401.
#[utoipa::path(post, path = "/auth/login", tag = "auth",
    request_body = Login, responses(
        (status = 200, body = Session, description = "New expiring session"),
        (status = 400, response = ErrorResponse),
        (status = 401, response = ErrorResponse),
        (status = 413, response = ErrorResponse),
        (status = 415, response = ErrorResponse),
        (status = 422, response = ErrorResponse),
        (status = 429, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn login(
    State(state): State<SessionState>,
    input: Result<Json<Login>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(input) = input?;
    let password = Zeroizing::new(input.password);
    if auth::password::validate(&password).is_err() {
        return Err(ApiError::Unauthorized);
    }
    let permit = auth::password::admit()?;
    let pool = &state.pool;
    let name = auth::name(&input.name).unwrap_or_default();
    let credential = db::sessions::credential(pool, &name).await?;
    let hash = credential.as_ref().map(|value| value.hash.clone());
    if !auth::password::verify(password, hash, permit).await? {
        return Err(ApiError::Unauthorized);
    }
    let credential = credential.ok_or(ApiError::Unauthorized)?;
    let token = auth::session_token()?;
    let expires_at = db::sessions::create(
        pool,
        &credential,
        &auth::digest(&token),
        state.auth.session_ttl_seconds,
    )
    .await?
    .ok_or(ApiError::Unauthorized)?;
    Ok((
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(Session {
            access_token: token,
            token_type: "Bearer",
            expires_at,
        }),
    )
        .into_response())
}

/// Revoke the presented active session; personal keys require CLI revocation.
#[utoipa::path(post, path = "/auth/logout", tag = "auth",
    security(("personal_key" = [])), responses(
        (status = 204, description = "Current session revoked"),
        (status = 401, response = ErrorResponse),
        (status = 500, response = ErrorResponse)
    ))]
async fn logout(
    auth::CurrentSession(digest): auth::CurrentSession,
    State(state): State<SessionState>,
) -> Result<StatusCode, ApiError> {
    db::sessions::revoke(&state.pool, &digest).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    //! Full router contracts for login, protected access, and session logout.

    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    /// Send requests through the production router without network sockets.
    async fn request(
        app: &Router,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request
                .header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some(body) => {
                request =
                    request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        app.clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap()
    }

    /// Decode a bounded response; tokens stay in test memory only.
    async fn json_body(response: Response) -> Value {
        serde_json::from_slice(
            &to_bytes(response.into_body(), 128 * 1024).await.unwrap(),
        )
        .unwrap()
    }

    /// A session authenticates ordinary routes and can be independently ended.
    #[tokio::test]
    async fn password_login_logout_and_roles_use_real_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let pool = db::connect(&directory.path().join("catalog.db"))
            .await
            .unwrap();
        let key = auth::token().unwrap();
        db::users::create(&pool, "alice", &auth::digest(&key))
            .await
            .unwrap();
        db::users::create(&pool, "key-only", &auth::digest("other"))
            .await
            .unwrap();
        let hash = auth::password::hash(Zeroizing::new(
            "  a strong password  ".into(),
        ))
        .await
        .unwrap();
        db::sessions::set_password(&pool, "alice", &hash)
            .await
            .unwrap();
        let scans = crate::jobs::ScanService::new(pool.clone(), vec![]);
        let app = crate::api::router(
            &AuthConfig {
                session_ttl_seconds: 3600,
            },
            &crate::config::DocsConfig { enabled: true },
            &pool,
            scans,
        );
        let mut expected = None;
        for (name, password) in [
            ("alice", "wrong password"),
            ("missing", "wrong password"),
            ("key-only", "wrong password"),
            ("../invalid", "wrong password"),
            ("alice", "a strong password"),
        ] {
            let response = request(
                &app,
                "POST",
                "/api/v1/auth/login",
                None,
                Some(json!({"name": name, "password": password})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
            let body = json_body(response).await;
            assert_eq!(expected.get_or_insert(body.clone()), &body);
        }
        let mut tokens = Vec::new();
        for _ in 0..2 {
            let response = request(
                &app,
                "POST",
                "/api/v1/auth/login",
                None,
                Some(json!({"name": " Alice ",
                    "password": "  a strong password  "})),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let body = json_body(response).await;
            assert_eq!(body["token_type"], "Bearer");
            let now: i64 = sqlx::query_scalar("SELECT unixepoch()")
                .fetch_one(&pool)
                .await
                .unwrap();
            let expiry = body["expires_at"].as_i64().unwrap();
            assert!((now + 3590..=now + 3600).contains(&expiry));
            let token = body["access_token"].as_str().unwrap().to_owned();
            assert!(token.starts_with("shs_"));
            let stored: String = sqlx::query_scalar(
                "SELECT digest FROM user_sessions WHERE digest = ?",
            )
            .bind(auth::digest(&token))
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_ne!(stored, token);
            tokens.push(token);
        }
        assert_ne!(tokens[0], tokens[1]);
        let first = &tokens[0];
        let second = &tokens[1];
        let me = request(&app, "GET", "/api/v1/me", Some(first), None).await;
        assert_eq!(json_body(me).await["name"], "alice");
        assert_eq!(
            request(
                &app,
                "GET",
                "/api/v1/library/duplicates",
                Some(first),
                None
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        db::users::set_role(&pool, "alice", auth::Role::Admin)
            .await
            .unwrap();
        assert_eq!(
            request(
                &app,
                "GET",
                "/api/v1/library/duplicates",
                Some(first),
                None
            )
            .await
            .status(),
            StatusCode::OK
        );
        db::users::set_role(&pool, "alice", auth::Role::Member)
            .await
            .unwrap();
        assert_eq!(
            request(
                &app,
                "GET",
                "/api/v1/library/duplicates",
                Some(first),
                None
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&app, "POST", "/api/v1/auth/logout", Some(&key), None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let response =
            request(&app, "POST", "/api/v1/auth/logout", Some(first), None)
                .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            to_bytes(response.into_body(), 1024)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            request(&app, "GET", "/api/v1/me", Some(first), None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        for token in [second, &key] {
            assert_eq!(
                request(&app, "GET", "/api/v1/me", Some(token), None)
                    .await
                    .status(),
                StatusCode::OK
            );
        }
        db::sessions::set_password(&pool, "alice", &hash)
            .await
            .unwrap();
        assert_eq!(
            request(&app, "GET", "/api/v1/me", Some(second), None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&app, "GET", "/api/v1/me", Some(&key), None)
                .await
                .status(),
            StatusCode::OK
        );
        let schema =
            json_body(request(&app, "GET", "/openapi.json", None, None).await)
                .await;
        let login = &schema["paths"]["/api/v1/auth/login"]["post"];
        assert!(login.get("security").is_none());
        for status in ["401", "413", "422", "429", "500"] {
            assert!(login["responses"][status].is_object());
        }
        let logout = &schema["paths"]["/api/v1/auth/logout"]["post"];
        assert!(logout["security"][0]["personal_key"].is_array());
        let input = &schema["components"]["schemas"]["Login"];
        assert_eq!(input["properties"]["password"]["format"], "password");
        pool.close().await;
    }

    /// Malformed input and excessive bodies use the shared JSON error format.
    #[tokio::test]
    async fn login_rejects_invalid_payloads_without_database_access() {
        let pool = SqlitePool::connect_lazy("sqlite::memory:").unwrap();
        let (app, _) = router(pool, AuthConfig::default()).split_for_parts();
        for (body, status) in [
            (json!({}), StatusCode::UNPROCESSABLE_ENTITY),
            (
                json!({
                    "name": "a", "password": "long password",
                    "role": "admin"
                }),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                json!({"name":"a", "password":"short"}),
                StatusCode::UNAUTHORIZED,
            ),
            (
                json!({"name":"a", "password":"x".repeat(5000)}),
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ] {
            let response =
                request(&app, "POST", "/auth/login", None, Some(body)).await;
            assert_eq!(response.status(), status);
            assert!(json_body(response).await["error"]["code"].is_string());
        }
        let response = request(&app, "POST", "/auth/login", None, None).await;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}
