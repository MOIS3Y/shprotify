//! Personal queue snapshots; positions distinguish repeated recordings.

use serde::Serialize;
use utoipa::ToSchema;

/// Maximum occurrences accepted in one queue, including duplicates.
pub(crate) const MAX_TRACKS: usize = 10_000;

/// A consistent snapshot of order and client-reported playback position.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Queue {
    /// Zero for the initial empty queue; every change increments the version.
    pub(crate) version: i64,
    /// One-based selected occurrence, or null when none is selected.
    pub(crate) current_position: Option<i64>,
    /// Media offset, not cumulative listening time; zero without a selection.
    pub(crate) position_ms: i64,
    /// All occurrences in order, with current catalog titles.
    pub(crate) items: Vec<QueueTrack>,
}

/// One occurrence in a queue; recording IDs may repeat at other positions.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct QueueTrack {
    /// Dense one-based position within this version of the queue.
    pub(crate) position: i64,
    /// Shared catalog identity, independent of files or playlists.
    pub(crate) recording_id: i64,
    /// Live title from the catalog.
    pub(crate) title: String,
}

/// Validate selection against the complete order; no duration is assumed.
pub(crate) fn validate_position(
    current: Option<i64>,
    position_ms: i64,
    count: usize,
) -> Result<(), &'static str> {
    if position_ms < 0 {
        return Err("position_ms must be nonnegative");
    }
    match current {
        Some(position) if position <= 0 || position as u64 > count as u64 => {
            Err("current_position must identify an occurrence in the queue")
        }
        None if position_ms != 0 => {
            Err("position_ms must be zero without a current_position")
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    //! Boundary checks independent of files and catalog duration.

    use super::*;

    /// A selection is an occurrence, and a cleared selection has no offset.
    #[test]
    fn validates_selection_and_offset() {
        for (current, offset, count) in
            [(None, 0, 0), (None, 0, 2), (Some(2), i64::MAX, 2)]
        {
            assert!(validate_position(current, offset, count).is_ok());
        }
        for (current, offset, count) in [
            (None, 1, 2),
            (Some(0), 0, 2),
            (Some(-1), 0, 2),
            (Some(1), 0, 0),
            (Some(3), 0, 2),
            (Some(1), -1, 2),
        ] {
            assert!(validate_position(current, offset, count).is_err());
        }
    }
}
