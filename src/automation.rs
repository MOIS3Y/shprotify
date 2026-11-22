//! Persisted interval schedules and bounded, explicit acquisition policies.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Compare-and-swap schedule update; interval is measured in elapsed seconds.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScheduleUpdate {
    /// Last observed settings version; zero creates a virtual schedule.
    pub(crate) version: i64,
    /// Disable future launches without cancelling already admitted work.
    pub(crate) enabled: bool,
    /// Desired interval within the instance's policy bounds.
    pub(crate) interval_seconds: u32,
}

/// Private schedule settings and its most recent automatic job outcome.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct Schedule {
    /// Settings revision; execution/status changes do not change this value.
    pub(crate) version: i64,
    /// Requested enabled state, retained while global automation is off.
    pub(crate) enabled: bool,
    /// Stored user-selected interval.
    pub(crate) interval_seconds: u32,
    /// Lower interval bound clients may offer for this target.
    #[sqlx(default)]
    pub(crate) min_interval_seconds: u32,
    /// Upper interval bound, currently thirty days.
    #[sqlx(default)]
    pub(crate) max_interval_seconds: u32,
    /// Current interval after applying the administrator's lower bound.
    #[sqlx(default)]
    pub(crate) effective_interval_seconds: u32,
    /// Global/configuration/permission suspension reason, if any.
    #[sqlx(default)]
    pub(crate) paused_reason: Option<String>,
    /// Unix seconds of the next attempt; null while disabled or active.
    pub(crate) next_run_at: Option<i64>,
    /// Unix seconds of the latest automatic dispatch attempt.
    pub(crate) last_run_at: Option<i64>,
    /// Idle, dispatching, running, completed, failed, deferred or paused.
    pub(crate) last_status: String,
    /// Native scan/discovery/metadata job ID in its usual API namespace.
    pub(crate) last_job_id: Option<i64>,
    /// Consecutive terminal failures; successful execution resets this count.
    pub(crate) failures: u32,
    /// Safe automatic-run diagnostic, independent of settings validation.
    pub(crate) message: Option<String>,
}

impl Schedule {
    /// Read settings without inserting a row merely because a client viewed
    /// it.
    pub(crate) fn disabled(interval: u32) -> Self {
        Self {
            version: 0,
            enabled: false,
            interval_seconds: interval,
            effective_interval_seconds: interval,
            min_interval_seconds: interval,
            max_interval_seconds: crate::config::automation::MAX_INTERVAL,
            paused_reason: None,
            next_run_at: None,
            last_run_at: None,
            last_status: "idle".into(),
            last_job_id: None,
            failures: 0,
            message: None,
        }
    }
}

/// Namespace and identity of one target; feeds remain scoped to their owner.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Target {
    /// Singleton external-library incremental scan, controlled by TOML.
    Library,
    /// An existing private Discovery feed.
    Discovery(i64),
    /// An administrator-selected artist/release/recording.
    Metadata(crate::library::enrichment::EntityKind, i64),
}

/// Expected schedule ownership/version errors remain distinct from SQL errors.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AutomationError {
    /// Unknown target, other account's private target, or removed schedule.
    #[error("Resource not found")]
    NotFound,
    /// Version race or enabled-schedule capacity reached.
    #[error("{0}")]
    Conflict(&'static str),
    /// Unsupported interval or malformed settings.
    #[error("{0}")]
    Invalid(&'static str),
    /// Storage failures stay in server diagnostics.
    #[error(transparent)]
    Storage(#[from] sqlx::Error),
}

/// Common durable-schedule result type.
pub(crate) type Result<T> = std::result::Result<T, AutomationError>;
