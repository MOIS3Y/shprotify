//! Finite managed-import choices; validation and identity safety stay
//! mandatory.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Exact source/content duplicate handling, applied again inside commit.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Deserialize,
    Serialize,
    ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DuplicatePolicy {
    /// Keep a single verified physical artifact and reuse the existing
    /// recording.
    #[default]
    Reuse,
    /// Reject acquisition of a source or complete byte hash already
    /// registered.
    Reject,
}

/// Explicit import policy without expressions, commands or provider
/// heuristics.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportPolicy {
    /// Decide whether verified exact duplicates can be reused.
    pub(crate) duplicates: DuplicatePolicy,
    /// Allow user-selected unknown tracks to create a shared Recording.
    /// False requires an existing target or previously linked candidate.
    pub(crate) create_recordings: bool,
    /// Allow another file for a recording that already has registered audio.
    /// Exact reused duplicates do not count as another file.
    pub(crate) allow_alternate_files: bool,
}

impl Default for ImportPolicy {
    /// Preserve the original explicit-selection import behavior.
    fn default() -> Self {
        Self {
            duplicates: DuplicatePolicy::Reuse,
            create_recordings: true,
            allow_alternate_files: true,
        }
    }
}
