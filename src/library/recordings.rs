//! Musical recordings independent of releases, track positions, and files.

use serde::Serialize;
use utoipa::ToSchema;

/// A distinct recording; matching titles do not imply matching identities.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Recording {
    /// Stable positive identifier assigned by SQLite.
    #[schema(minimum = 1)]
    pub(crate) id: i64,
    /// Trimmed display title; 1–255 Unicode scalar values without controls.
    pub(crate) title: String,
    /// Ordered credited artists; may be empty when credits are unknown.
    pub(crate) artist_ids: Vec<i64>,
}

/// Validated creation input, with no release or audio file dependency.
#[derive(Debug)]
pub(crate) struct NewRecording {
    /// Trimmed display title.
    pub(crate) title: String,
    /// Up to 100 distinct positive artist IDs in display order.
    pub(crate) artist_ids: Vec<i64>,
}

impl NewRecording {
    /// Validate the title and credits before entering a database transaction.
    pub(crate) fn new(
        title: &str,
        artist_ids: Vec<i64>,
    ) -> Result<Self, &'static str> {
        let title = super::validate_title(title)?;
        super::validate_artist_ids(&artist_ids)?;
        Ok(Self {
            title: title.into(),
            artist_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    //! Recording normalization, unknown credits, and invalid input boundaries.

    use super::*;

    /// Unicode titles are trimmed and credit order is preserved.
    #[test]
    fn normalizes_title_and_keeps_credit_order() {
        let recording = NewRecording::new("  Запись  ", vec![2, 1]).unwrap();
        assert_eq!(recording.title, "Запись");
        assert_eq!(recording.artist_ids, [2, 1]);
        assert!(NewRecording::new(&"я".repeat(255), vec![]).is_ok());
    }

    /// Blank or oversized titles, controls, and invalid credits are rejected.
    #[test]
    fn rejects_invalid_titles_and_credits() {
        for title in ["".into(), " \n ".into(), "a\0b".into(), "я".repeat(256)]
        {
            assert!(NewRecording::new(&title, vec![]).is_err());
        }
        for ids in [vec![1, 1], vec![0], vec![-1], (1..=101).collect()] {
            assert!(NewRecording::new("Title", ids).is_err());
        }
    }
}
