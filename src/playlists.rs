//! Owned playlists with optional publication inside the instance.

use serde::Serialize;
use utoipa::ToSchema;

/// Bound full-list edits so validation and transactions have predictable cost.
pub(crate) const MAX_TRACKS: usize = 10_000;

/// Validated playlist metadata; names need not be unique.
#[derive(Debug)]
pub(crate) struct PlaylistDetails {
    /// Trimmed display name without control characters.
    pub(crate) name: String,
    /// Trimmed free text, allowing line breaks and tabs.
    pub(crate) description: String,
}

impl PlaylistDetails {
    /// Validate Unicode lengths without altering internal whitespace.
    pub(crate) fn new(
        name: &str,
        description: &str,
    ) -> Result<Self, &'static str> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > 255
            || name.chars().any(char::is_control)
        {
            return Err("Name must be 1–255 characters without controls");
        }
        let description = description.trim();
        if description.chars().count() > 2000
            || description
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        {
            return Err("Description must be at most 2000 text characters");
        }
        Ok(Self {
            name: name.into(),
            description: description.into(),
        })
    }
}

/// Validate an ordered list of recording occurrences, including duplicates.
pub(crate) fn validate_tracks(recordings: &[i64]) -> Result<(), &'static str> {
    if recordings.len() > MAX_TRACKS || recordings.iter().any(|id| *id <= 0) {
        return Err("Expected at most 10000 positive recording IDs");
    }
    Ok(())
}

/// Playlist metadata; publication permits authenticated, read-only discovery.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct Playlist {
    /// Stable account-scoped resource ID.
    pub(crate) id: i64,
    /// User-selected display name, including names shared by other playlists.
    pub(crate) name: String,
    /// Free-text description, empty when omitted on create/metadata
    /// replacement.
    pub(crate) description: String,
    /// Whether other authenticated accounts may read this playlist.
    pub(crate) is_public: bool,
    /// Positive mutation version; supply it for edits and deletion.
    pub(crate) version: i64,
    /// Server creation time, UTC ISO 8601 with milliseconds.
    pub(crate) created_at: String,
    /// Last effective mutation time; the version distinguishes timestamp ties.
    pub(crate) updated_at: String,
}

/// One ordered occurrence of a recording in a playlist.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct PlaylistTrack {
    /// One-based position, dense within the current playlist version.
    pub(crate) position: i64,
    /// Shared recording; duplicates at different positions are allowed.
    pub(crate) recording_id: i64,
    /// Current shared catalog title, without copying or changing metadata.
    pub(crate) title: String,
}

#[cfg(test)]
mod tests {
    //! Unicode metadata boundaries and bounded lists of recording occurrences.

    use super::*;

    /// Names/description are normalized without imposing unique playlist
    /// names.
    #[test]
    fn validates_metadata_and_preserves_description_lines() {
        let details =
            PlaylistDetails::new("  Дорога  ", " one\n\ttwo ").unwrap();
        assert_eq!(details.name, "Дорога");
        assert_eq!(details.description, "one\n\ttwo");
        for name in ["", "  ", "bad\nname", &"я".repeat(256)] {
            assert!(PlaylistDetails::new(name, "").is_err());
        }
        assert!(PlaylistDetails::new(&"я".repeat(255), "").is_ok());
        assert!(PlaylistDetails::new("name", &"я".repeat(2000)).is_ok());
        for description in ["bad\0text", &"я".repeat(2001)] {
            assert!(PlaylistDetails::new("name", description).is_err());
        }
    }

    /// Empty lists clear a playlist; repetitions represent separate
    /// occurrences.
    #[test]
    fn validates_track_lists_without_deduplicating_them() {
        assert!(validate_tracks(&[]).is_ok());
        assert!(validate_tracks(&[1, 2, 1]).is_ok());
        assert!(validate_tracks(&vec![1; MAX_TRACKS]).is_ok());
        assert!(validate_tracks(&vec![1; MAX_TRACKS + 1]).is_err());
        assert!(validate_tracks(&[0]).is_err());
        assert!(validate_tracks(&[-1]).is_err());
    }
}
