//! Salted Argon2id passwords; expensive work never runs on Tokio workers.

use crate::api::error::ApiError;
use anyhow::Context;
use argon2::{Argon2, PasswordHasher, PasswordVerifier, password_hash};
use tokio::sync::{Semaphore, SemaphorePermit};
use zeroize::Zeroizing;

/// Bound memory and blocking tasks, including work outliving cancelled HTTP.
static WORKERS: Semaphore = Semaphore::const_new(2);

/// Maximum UTF-8 password size; input is never silently truncated.
pub(crate) const MAX_BYTES: usize = 1024;

/// Require 12–1024 UTF-8 bytes without control characters; preserve spaces.
pub(crate) fn validate(password: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        (12..=MAX_BYTES).contains(&password.len())
            && !password.chars().any(char::is_control),
        "Password must be 12–1024 UTF-8 bytes without control characters"
    );
    Ok(())
}

/// Admit HTTP verification without building an unbounded queue of secrets.
pub(crate) fn admit() -> Result<SemaphorePermit<'static>, ApiError> {
    WORKERS.try_acquire().map_err(|_| ApiError::TooManyRequests)
}

/// Hash with Argon2id v19, 19 MiB, two passes, one lane and a random salt.
fn encode(password: &str) -> anyhow::Result<String> {
    let hash = Argon2::default()
        .hash_password(password.as_bytes())
        .context("Failed to hash password")?;
    Ok(hash.to_string())
}

/// Hash validated CLI input off-thread, clearing its buffer on completion.
pub(crate) async fn hash(
    password: Zeroizing<String>,
) -> anyhow::Result<String> {
    validate(&password)?;
    let permit = WORKERS.acquire().await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        encode(&password)
    })
    .await?
}

/// Verify a snapshot off-thread; unknown accounts still perform Argon2 work.
/// The permit stays with the blocking task if the request is cancelled.
pub(crate) async fn verify(
    password: Zeroizing<String>,
    hash: Option<String>,
    permit: SemaphorePermit<'static>,
) -> anyhow::Result<bool> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let Some(hash) = hash else {
            encode(&password)?;
            return Ok(false);
        };
        match Argon2::default().verify_password(password.as_bytes(), &*hash) {
            Ok(()) => Ok(true),
            Err(password_hash::Error::PasswordInvalid) => Ok(false),
            Err(error) => Err(anyhow::Error::new(error)
                .context("Failed to verify stored password")),
        }
    })
    .await?
}

#[cfg(test)]
mod tests {
    //! Password boundaries and fresh Argon2id salts.

    use super::*;

    /// Password bytes are preserved and invalid input is rejected explicitly.
    #[test]
    fn validates_password_boundaries() {
        for input in ["", "short", "long enough\n", &"a".repeat(1025)] {
            assert!(validate(input).is_err());
        }
        for input in ["  some spaces  ", "длинный пароль", &"x".repeat(1024)]
        {
            assert!(validate(input).is_ok());
        }
    }

    /// Every encoding has a fresh salt and the selected Argon2id parameters.
    #[tokio::test]
    async fn hashes_use_random_salts() {
        let first = hash(Zeroizing::new("a strong password".into()))
            .await
            .unwrap();
        let second = hash(Zeroizing::new("a strong password".into()))
            .await
            .unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    }
}
