//! Account provisioning, password authentication and session lifecycle.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};
use reqwest::{Method, StatusCode, header};
use serde_json::json;

use crate::support::{Fixture, string};

/// Bootstrap through CLI, manage password accounts and revoke real sessions.
#[tokio::test]
async fn password_login_permissions_and_logout() -> Result<()> {
    let mut server = Fixture::new(false).await?;
    server
        .cli(
            &["user", "password", "--name", "operator", "--password-stdin"],
            Some("operator password\n"),
        )
        .await?;
    server.start().await?;
    let login = server
        .json(
            Method::POST,
            "/api/v1/auth/login",
            "",
            Some(json!({"name":"operator", "password":"operator password"})),
            StatusCode::OK,
        )
        .await?;
    assert_eq!(login["token_type"], "Bearer");
    let now = i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?;
    let remaining = login["expires_at"].as_i64().unwrap() - now;
    ensure!(
        (3500..=3600).contains(&remaining),
        "Configured session TTL ignored"
    );
    let session = string(&login, "access_token")?;
    let me = server
        .json(Method::GET, "/api/v1/me", session, None, StatusCode::OK)
        .await?;
    assert_eq!(me["role"], "admin");
    let alice = server
        .json(
            Method::POST,
            "/api/v1/users",
            session,
            Some(json!({"name":"alice", "password":"alice password"})),
            StatusCode::CREATED,
        )
        .await?;
    assert_eq!(alice["role"], "member");
    let login = server
        .json(
            Method::POST,
            "/api/v1/auth/login",
            "",
            Some(json!({"name":"alice", "password":"alice password"})),
            StatusCode::OK,
        )
        .await?;
    let alice_session = string(&login, "access_token")?;
    server.stop().await?;
    server.start().await?;
    assert_eq!(
        server.get("/api/v1/me", alice_session).await?["name"],
        "alice"
    );
    assert_eq!(server.get("/api/v1/me", session).await?["role"], "admin");
    server
        .json(
            Method::GET,
            "/api/v1/users",
            alice_session,
            None,
            StatusCode::FORBIDDEN,
        )
        .await?;
    server
        .json(
            Method::POST,
            "/api/v1/auth/login",
            "",
            Some(json!({"name":"alice", "password":"wrong password"})),
            StatusCode::UNAUTHORIZED,
        )
        .await?;
    let response = server
        .response(
            server.request(Method::POST, "/api/v1/auth/logout", alice_session),
            StatusCode::NO_CONTENT,
        )
        .await?;
    ensure!(response.bytes().await?.is_empty());
    let response = server
        .response(
            server.request(Method::GET, "/api/v1/me", alice_session),
            StatusCode::UNAUTHORIZED,
        )
        .await?;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    server.stop().await
}
