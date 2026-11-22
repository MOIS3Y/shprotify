//! Personal bearer-key authentication; only credential hashes reach storage.

use crate::{api::error::ApiError, db};
use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use utoipa::ToSchema;

/// Public account identity without credential information.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct User {
    /// Stable identity for personal data.
    pub(crate) id: i64,
    /// Unique normalized ASCII account name.
    pub(crate) name: String,
}

/// Principal derived only from an active bearer key, never a supplied user ID.
pub(crate) struct CurrentUser(pub(crate) User);

impl FromRequestParts<SqlitePool> for CurrentUser {
    type Rejection = ApiError;

    /// Reject absent, malformed, duplicate, and revoked credentials.
    async fn from_request_parts(
        parts: &mut Parts,
        pool: &SqlitePool,
    ) -> Result<Self, Self::Rejection> {
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
            || !token.starts_with("shp_")
            || !token[4..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(ApiError::Unauthorized);
        }
        Ok(Self(
            db::users::authenticate(pool, &digest(token))
                .await?
                .ok_or(ApiError::Unauthorized)?,
        ))
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
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("OS randomness is unavailable"))?;
    Ok(format!("shp_{}", hex(&bytes)))
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
