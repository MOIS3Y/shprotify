//! Optional acquisition contracts, independent of the experimental provider.
//!
//! A selected source is a user assertion, not an acoustic identity proof.
//! Only the server validates and publishes files into managed storage.

pub(crate) mod storage;

use crate::discovery::CandidateData;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Source resolution input; no arbitrary URL, path or executable arguments.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SearchRequest {
    /// Acquire a file for an existing recording without changing its metadata.
    Recording {
        /// Shared recording identity.
        id: i64,
    },
    /// Find audio for a recording candidate visible to the current account.
    Candidate {
        /// Discovery identity; releases/playlists are not single audio files.
        id: i64,
    },
    /// Search explicitly without creating shared catalog rows yet.
    Query {
        /// Desired recording title, at most 255 characters.
        title: String,
        /// Ordered display credits, at most 20.
        artists: Vec<String>,
    },
}

/// Durable intent captured before calling an external provider.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Target {
    /// Original client intent, used to revalidate manual retries.
    pub(crate) request: SearchRequest,
    /// Desired display title; source video titles do not overwrite it.
    pub(crate) title: String,
    /// Desired ordered display credits, independent of embedded source tags.
    pub(crate) artists: Vec<String>,
    /// Known shared recording, if resolved at admission time.
    pub(crate) recording_id: Option<i64>,
    /// Candidate used for feedback reconciliation after acquisition.
    pub(crate) candidate_id: Option<i64>,
    /// Approximate source duration selected by the user, if known.
    #[serde(default)]
    pub(crate) source_duration_seconds: Option<i64>,
    /// Snapshot detects candidate identity changes before import.
    pub(crate) candidate: Option<CandidateData>,
}

impl Target {
    /// Check bounded human metadata before provider invocation or SQL writes.
    pub(crate) fn validate(&self) -> Result<()> {
        text(&self.title)?;
        if self.artists.len() > 100 {
            return Err(DownloadError::Invalid("Too many artist credits"));
        }
        for artist in &self.artists {
            text(artist)?;
        }
        Ok(())
    }

    /// Stable active-job identity; source and target uniqueness are separate.
    pub(crate) fn key(&self) -> String {
        if let Some(id) = self.recording_id {
            format!("recording:{id}")
        } else if let Some(id) = self.candidate_id {
            format!("candidate:{id}")
        } else {
            crate::discovery::digest(
                &format!("{}\n{:?}", self.title, self.artists).to_lowercase(),
            )
        }
    }
}

/// Validate one display field without modifying non-ASCII spelling.
pub(crate) fn text(value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.chars().count() > 255
        || value.chars().any(char::is_control)
    {
        return Err(DownloadError::Invalid(
            "Display fields must be 1–255 characters without controls",
        ));
    }
    Ok(())
}

/// Bounded, validated YouTube search result, never a temporary CDN URL.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub(crate) struct Source {
    /// Selection identity scoped to an owned search job.
    pub(crate) id: i64,
    /// Parent search job identity.
    pub(crate) job_id: i64,
    /// Stable YouTube video ID.
    pub(crate) video_id: String,
    /// Stable public source page, constructed from a validated video ID.
    pub(crate) url: String,
    /// Display title supplied by YouTube, not trusted catalog metadata.
    pub(crate) title: String,
    /// Display channel, if supplied by the provider.
    pub(crate) channel: Option<String>,
    /// Approximate duration; actual imported audio is checked separately.
    pub(crate) duration_seconds: Option<i64>,
}

/// Persisted, owned search/download state; paths and subprocess logs stay
/// private.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct DownloadJob {
    /// Durable positive job ID.
    pub(crate) id: i64,
    /// `search` or `download`.
    pub(crate) operation: String,
    /// Queued, running, verifying, importing, or a terminal state.
    pub(crate) status: String,
    /// YouTube ID selected for a download; null for searches.
    pub(crate) video_id: Option<String>,
    /// Previous failed/cancelled/interrupted job for an explicit retry.
    pub(crate) retry_of: Option<i64>,
    /// Whether the owner requested cancellation before the import boundary.
    pub(crate) cancel_requested: bool,
    /// Bytes most recently observed on disk; may include a partial container.
    pub(crate) downloaded_bytes: i64,
    /// Expected encoded size, when the provider reports it reliably.
    pub(crate) total_bytes: Option<i64>,
    /// Imported or reused catalog recording after completion.
    pub(crate) recording_id: Option<i64>,
    /// Imported or reused physical file after completion.
    pub(crate) file_id: Option<i64>,
    /// Creation time in Unix seconds.
    pub(crate) created_at: i64,
    /// Terminal time in Unix seconds.
    pub(crate) finished_at: Option<i64>,
    /// Safe status explanation, without paths, cookies or provider output.
    pub(crate) message: Option<String>,
}

/// Typed acquisition failures; only static domain messages reach HTTP clients.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DownloadError {
    /// Owned resource is absent or not visible to this account.
    #[error("Resource not found")]
    NotFound,
    /// Invalid client intent or a disabled adapter.
    #[error("{0}")]
    Invalid(&'static str),
    /// Queue capacity, duplicate work, stale intent or cancellation boundary.
    #[error("{0}")]
    Conflict(&'static str),
    /// Expected isolated provider, media validation or filesystem failure.
    #[error("{0}")]
    Failed(&'static str),
    /// Unexpected database error, logged only by the server.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
    /// Unexpected serialization error, logged only by the server.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Unexpected filesystem error, logged only by the server.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Domain result shared by the provider, worker and importer.
pub(crate) type Result<T> = std::result::Result<T, DownloadError>;

/// Stable video IDs are the only input used to construct YouTube URLs.
pub(crate) fn video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
