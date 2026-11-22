//! Media credential lifetime and the immutable HTTP client-access projection.

use serde::{Deserialize, Serialize};

/// Browser media uses scoped opaque tickets, independent of general sessions.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlaybackConfig {
    /// Reusable media-link lifetime, 1–3600 seconds; parent expiry still
    /// applies.
    pub(crate) ticket_ttl_seconds: u32,
}
impl Default for PlaybackConfig {
    /// Five minutes is enough for initial playback/seek; clients can renew.
    fn default() -> Self {
        Self {
            ticket_ttl_seconds: 300,
        }
    }
}

/// Safe startup policy shared by HTTP routers, never containing provider keys.
#[derive(Debug, Clone)]
pub(crate) struct ClientSettings {
    /// Preserve public catalog reads unless the operator explicitly closes
    /// them.
    pub(crate) public_catalog: bool,
    /// Exact browser origins; empty means no cross-origin browser access.
    pub(crate) cors_origins: Vec<String>,
    /// Scoped media ticket lifetime.
    pub(crate) ticket_ttl_seconds: u32,
}
impl Default for ClientSettings {
    /// Match the server's default public-read and same-origin deployment
    /// policy.
    fn default() -> Self {
        Self {
            public_catalog: true,
            cors_origins: vec![],
            ticket_ttl_seconds: 300,
        }
    }
}
