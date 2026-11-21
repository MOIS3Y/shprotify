//! Release entities and validation independent of HTTP and persistence.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Primary release classification, stored and serialized as lowercase text.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, sqlx::Type, ToSchema)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub(crate) enum ReleaseKind {
    /// Full-length album.
    Album,
    /// Single release.
    Single,
    /// Extended play release.
    Ep,
    /// Collection of recordings, potentially by multiple artists.
    Compilation,
    /// Release outside the primary categories.
    Other,
}

/// A release in the shared catalog, independent of recordings and files.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Release {
    /// Stable positive identifier assigned by SQLite.
    #[schema(minimum = 1)]
    pub(crate) id: i64,
    /// Trimmed display title; titles are not unique.
    pub(crate) title: String,
    /// Primary release classification.
    pub(crate) kind: ReleaseKind,
    /// Credited artists in caller-supplied order; may be empty.
    pub(crate) artist_ids: Vec<i64>,
}

/// Validated input for atomic creation of a release and its artist credits.
#[derive(Debug)]
pub(crate) struct NewRelease {
    /// Trimmed title containing 1–255 Unicode scalar values.
    pub(crate) title: String,
    /// Primary release classification.
    pub(crate) kind: ReleaseKind,
    /// At most 100 distinct positive artist identifiers in display order.
    pub(crate) artist_ids: Vec<i64>,
}

impl NewRelease {
    /// Validate the title and credits without checking database existence.
    pub(crate) fn new(
        title: &str,
        kind: ReleaseKind,
        artist_ids: Vec<i64>,
    ) -> Result<Self, &'static str> {
        let title = super::validate_title(title)?;
        super::validate_artist_ids(&artist_ids)?;
        Ok(Self {
            title: title.into(),
            kind,
            artist_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    //! Title, credit uniqueness, and Unicode boundary validation.

    use super::*;

    /// Unicode titles and intentionally absent artist credits are valid.
    #[test]
    fn normalizes_title_and_preserves_credit_order() {
        let release = NewRelease::new(
            "  Сборник  ",
            ReleaseKind::Compilation,
            vec![2, 1],
        )
        .unwrap();
        assert_eq!(release.title, "Сборник");
        assert_eq!(release.artist_ids, [2, 1]);
        assert!(
            NewRelease::new(&"я".repeat(255), ReleaseKind::Album, vec![])
                .is_ok()
        );
    }

    /// Invalid input is rejected before a transaction can create rows.
    #[test]
    fn rejects_invalid_titles_and_credits() {
        for title in ["".into(), " \n ".into(), "a\0b".into(), "я".repeat(256)]
        {
            assert!(
                NewRelease::new(&title, ReleaseKind::Album, vec![]).is_err()
            );
        }
        for ids in [vec![1, 1], vec![0], vec![-1], (1..=101).collect()] {
            assert!(
                NewRelease::new("Title", ReleaseKind::Album, ids).is_err()
            );
        }
    }
}
