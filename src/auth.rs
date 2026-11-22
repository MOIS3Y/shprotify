//! Bearer authentication for personal keys and expiring password sessions.

pub(crate) mod password;

use crate::{api::error::ApiError, db};
use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use utoipa::ToSchema;

/// Server-side permissions checked from SQLite on each authenticated request.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    sqlx::Type,
    ToSchema,
    clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub(crate) enum Role {
    /// Personal preferences; no authority to modify the shared catalog.
    #[default]
    Member,
    /// Shared catalog writes, scanning, and administrative reports.
    Admin,
}

/// Public account identity without credential information.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct User {
    /// Stable identity for personal data.
    pub(crate) id: i64,
    /// Unique normalized ASCII account name.
    pub(crate) name: String,
    /// Current permissions, independent of which key is used.
    pub(crate) role: Role,
}

/// Principal from an active key or session, never a supplied user ID.
pub(crate) struct CurrentUser(pub(crate) User);

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
    SqlitePool: FromRef<S>,
{
    type Rejection = ApiError;

    /// Reject absent, malformed, duplicate, and revoked credentials.
    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(parts)?;
        Ok(Self(
            db::users::authenticate(
                &SqlitePool::from_ref(state),
                &digest(token),
            )
            .await?
            .ok_or(ApiError::Unauthorized)?,
        ))
    }
}

/// Authenticated session digest; personal keys cannot be logged out.
pub(crate) struct CurrentSession(pub(crate) String);

impl<S> FromRequestParts<S> for CurrentSession
where
    S: Send + Sync,
    SqlitePool: FromRef<S>,
{
    type Rejection = ApiError;

    /// Require an active password session rather than a personal key.
    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        if !bearer(parts)?.starts_with("shs_") {
            return Err(ApiError::Unauthorized);
        }
        CurrentUser::from_request_parts(parts, state).await?;
        Ok(Self(digest(bearer(parts)?)))
    }
}

/// Parse exactly one Bearer header without accepting alternate transports.
fn bearer(parts: &Parts) -> Result<&str, ApiError> {
    let mut headers = parts.headers.get_all(header::AUTHORIZATION).iter();
    let value = headers
        .next()
        .and_then(|h| h.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    if headers.next().is_some() {
        return Err(ApiError::Unauthorized);
    }
    let (scheme, token) =
        value.split_once(' ').ok_or(ApiError::Unauthorized)?;
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.len() != 68
        || !(token.starts_with("shp_") || token.starts_with("shs_"))
        || !token[4..].bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(token)
}

/// Guard for administrative operations; client role claims are never trusted.
pub(crate) struct Admin;

impl<S> FromRequestParts<S> for Admin
where
    S: Send + Sync,
    SqlitePool: FromRef<S>,
{
    type Rejection = ApiError;

    /// Authenticate first (401), then reject insufficient privileges (403).
    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let CurrentUser(user) =
            CurrentUser::from_request_parts(parts, state).await?;
        if user.role != Role::Admin {
            return Err(ApiError::Forbidden);
        }
        Ok(Self)
    }
}

/// Validate a local account name without Unicode case ambiguity.
pub(crate) fn name(value: &str) -> anyhow::Result<String> {
    let value = value.trim().to_ascii_lowercase();
    anyhow::ensure!(
        !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|b| b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || b"._-".contains(&b)),
        "Account name must be 1–64 ASCII letters, digits, '.', '_' or '-'"
    );
    Ok(value)
}

/// Generate a one-time CLI credential containing 256 bits of OS randomness.
pub(crate) fn token() -> anyhow::Result<String> {
    random_token("shp_")
}

/// Generate a session credential with a distinct, non-authoritative prefix.
pub(crate) fn session_token() -> anyhow::Result<String> {
    random_token("shs_")
}

/// Combine a credential kind with 256 bits of operating system randomness.
fn random_token(prefix: &str) -> anyhow::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("OS randomness is unavailable"))?;
    Ok(format!("{prefix}{}", hex(&bytes)))
}

/// Hash a credential before persistence or lookup.
pub(crate) fn digest(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

/// Encode bytes without writing sensitive data to logs.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    //! Account validation and unpredictable one-time credential shapes.

    use super::*;

    /// Generated keys differ and their digests do not expose the secret.
    #[test]
    fn validates_account_names_and_generates_distinct_keys() {
        assert_eq!(name(" Alice ").unwrap(), "alice");
        for invalid in ["", "a b", "имя", "../", &"a".repeat(65)] {
            assert!(name(invalid).is_err());
        }
        let first = token().unwrap();
        let second = token().unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 68);
        assert_eq!(digest(&first).len(), 64);
        assert!(!digest(&first).contains(&first));
    }
}
