//! Simple opt-in interval policies; no cron, shell commands or time zones.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Largest supported interval, in seconds: thirty days.
pub(crate) const MAX_INTERVAL: u32 = 30 * 24 * 3600;

/// Global scheduler policy; disabled instances retain configured schedules.
#[derive(Debug, Default, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AutomationConfig {
    /// Allow automatic dispatch; manual operations remain independent.
    pub(crate) enabled: bool,
    /// External collection indexing interval.
    pub(crate) library: LibraryAutomation,
    /// User-owned feed schedule defaults and bounds.
    pub(crate) discovery: DiscoveryAutomation,
    /// Administrator-selected metadata proposal schedules.
    pub(crate) metadata: MetadataAutomation,
}

/// A zero interval disables the singleton external-library scan schedule.
#[derive(Debug, Default, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LibraryAutomation {
    /// Zero or 60 seconds through thirty days; scans use incremental mode.
    pub(crate) scan_interval_seconds: u32,
}

/// Personal schedules are explicitly enabled per existing feed through HTTP.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoveryAutomation {
    /// Interval proposed to clients for a disabled/virtual schedule.
    pub(crate) default_interval_seconds: u32,
    /// Lower bound for accepted and effective stored intervals.
    pub(crate) min_interval_seconds: u32,
    /// Maximum enabled feed schedules per account; 1–100.
    pub(crate) max_schedules_per_user: i64,
}

impl Default for DiscoveryAutomation {
    /// Six-hour refreshes, at least one hour apart, twenty per account.
    fn default() -> Self {
        Self {
            default_interval_seconds: 21600,
            min_interval_seconds: 3600,
            max_schedules_per_user: 20,
        }
    }
}

/// Only explicitly selected catalog objects produce automatic proposals.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MetadataAutomation {
    /// Additional opt-in; catalog fields and artwork are never auto-applied.
    pub(crate) enabled: bool,
    /// Default and minimum metadata interval, 3600 seconds through thirty
    /// days.
    pub(crate) interval_seconds: u32,
    /// Maximum enabled metadata targets across the instance; 1–100.
    pub(crate) max_schedules: i64,
}

impl Default for MetadataAutomation {
    /// Daily proposals for at most fifty explicitly selected targets.
    fn default() -> Self {
        Self {
            enabled: false,
            interval_seconds: 86400,
            max_schedules: 50,
        }
    }
}

impl AutomationConfig {
    /// Reject invalid resource limits even while dispatch is disabled.
    pub(super) fn validate(&self) -> Result<()> {
        let scan = self.library.scan_interval_seconds;
        ensure!(
            scan == 0 || (60..=MAX_INTERVAL).contains(&scan),
            "Invalid automation.library.scan_interval_seconds"
        );
        ensure!(
            (60..=MAX_INTERVAL).contains(&self.discovery.min_interval_seconds)
                && (self.discovery.min_interval_seconds..=MAX_INTERVAL)
                    .contains(&self.discovery.default_interval_seconds)
                && (1..=100).contains(&self.discovery.max_schedules_per_user),
            "Invalid automation.discovery limits"
        );
        ensure!(
            (3600..=MAX_INTERVAL).contains(&self.metadata.interval_seconds)
                && (1..=100).contains(&self.metadata.max_schedules),
            "Invalid automation.metadata limits"
        );
        Ok(())
    }
}
