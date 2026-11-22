//! Read-only external metadata adapters; no automatic catalog writes.

pub(crate) mod acoustid;
pub(crate) mod discovery;
pub(crate) mod musicbrainz;
pub(crate) mod youtube;

use anyhow::{Result, ensure};
use reqwest::{Client, Response, Url};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};

/// Shared HTTP transport with bounded bodies, deadlines and global pacing.
#[derive(Clone)]
pub(crate) struct Transport {
    /// Reused connection pool; redirects are confined to known HTTPS hosts.
    pub(crate) client: Client,
    /// Earliest next external call across every metadata adapter.
    next: Arc<Mutex<Instant>>,
}

impl Transport {
    /// Build an explicitly identified client without cookies or system proxy.
    pub(crate) fn new(user_agent: &str) -> Result<Self> {
        let client = Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .referer(false)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !allowed(attempt.url()) {
                    attempt.error("Disallowed provider redirect")
                } else if attempt.url().host_str() == Some("musicbrainz.org") {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()?;
        Ok(Self {
            client,
            next: provider_clock(),
        })
    }

    /// Space request starts at least one second apart, including failures.
    pub(crate) async fn pace(&self) {
        let mut next = self.next.lock().await;
        tokio::time::sleep_until(*next).await;
        *next = Instant::now() + Duration::from_secs(1);
    }

    /// Reject oversized bodies even when content length is absent or false.
    pub(crate) async fn bytes(
        mut response: Response,
        max: usize,
    ) -> Result<Vec<u8>> {
        ensure!(response.status().is_success(), "Provider request failed");
        ensure!(
            response.content_length().is_none_or(|n| n <= max as u64),
            "Provider body exceeds size limit"
        );
        let mut data = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                chunk.len() <= max.saturating_sub(data.len()),
                "Provider body exceeds size limit"
            );
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }
}

/// All production clients share pacing, including metadata and discovery.
fn provider_clock() -> Arc<Mutex<Instant>> {
    static CLOCK: OnceLock<Arc<Mutex<Instant>>> = OnceLock::new();
    CLOCK
        .get_or_init(|| Arc::new(Mutex::new(Instant::now())))
        .clone()
}

/// Redirects cannot reach arbitrary hosts, local addresses or plaintext URLs.
fn allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none_or(|p| p == 443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            [
                "musicbrainz.org",
                "coverartarchive.org",
                "api.acoustid.org",
                "archive.org",
                "ws.audioscrobbler.com",
                "api.listenbrainz.org",
            ]
            .contains(&host)
                || host.ends_with(".archive.org")
        })
}

#[cfg(test)]
mod tests {
    //! Redirect boundaries and pacing remain shared across adapter clones.
    use super::*;

    /// Known HTTPS archive redirects work; arbitrary URLs cannot be followed.
    #[test]
    fn rejects_disallowed_redirect_targets() {
        for url in [
            "https://archive.org/x",
            "https://ia800.us.archive.org/x",
            "https://coverartarchive.org/release/x",
        ] {
            assert!(allowed(&Url::parse(url).unwrap()));
        }
        for url in [
            "http://archive.org/x",
            "https://127.0.0.1/x",
            "https://archive.org.evil.test/x",
            "https://evilarchive.org/x",
            "https://archive.org:123/x",
            "https://user@archive.org/x",
        ] {
            assert!(!allowed(&Url::parse(url).unwrap()));
        }
    }
}
