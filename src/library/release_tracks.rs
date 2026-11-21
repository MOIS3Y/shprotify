//! Recording placement within releases, independent of physical files.

use serde::Serialize;
use utoipa::ToSchema;

/// One occurrence of a recording at a disc and position within a release.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct ReleaseTrack {
    /// Stable positive identifier of this occurrence.
    #[schema(minimum = 1)]
    pub(crate) id: i64,
    /// Parent release identifier.
    #[schema(minimum = 1)]
    pub(crate) release_id: i64,
    /// Shared recording, reusable in other releases or positions.
    #[schema(minimum = 1)]
    pub(crate) recording_id: i64,
    /// One-based disc number.
    #[schema(minimum = 1)]
    pub(crate) disc_number: i64,
    /// One-based position within the disc; gaps are allowed.
    #[schema(minimum = 1)]
    pub(crate) position: i64,
}

/// Validated placement; reference existence is checked during insertion.
#[derive(Debug)]
pub(crate) struct NewReleaseTrack {
    /// Recording to place in the release.
    pub(crate) recording_id: i64,
    /// Positive disc number.
    pub(crate) disc_number: i64,
    /// Positive position within the disc.
    pub(crate) position: i64,
}

impl NewReleaseTrack {
    /// Reject nonpositive identifiers, disc numbers, and positions.
    pub(crate) fn new(
        recording_id: i64,
        disc_number: i64,
        position: i64,
    ) -> Result<Self, &'static str> {
        if recording_id <= 0 || disc_number <= 0 || position <= 0 {
            return Err(
                "recording_id, disc_number and position must be positive",
            );
        }
        Ok(Self {
            recording_id,
            disc_number,
            position,
        })
    }
}

#[cfg(test)]
mod tests {
    //! Placement boundaries without persistence dependencies.

    use super::*;

    /// Only positive identifiers and one-based positions are accepted.
    #[test]
    fn validates_placement_coordinates() {
        assert!(NewReleaseTrack::new(1, 1, 1).is_ok());
        assert!(NewReleaseTrack::new(i64::MAX, i64::MAX, i64::MAX).is_ok());
        for (recording, disc, position) in [
            (0, 1, 1),
            (-1, 1, 1),
            (1, 0, 1),
            (1, -1, 1),
            (1, 1, 0),
            (1, 1, -1),
        ] {
            assert!(NewReleaseTrack::new(recording, disc, position).is_err());
        }
    }
}
