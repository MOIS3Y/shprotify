//! Shared music catalog entities and their validation rules.

pub(crate) mod releases;

use serde::Serialize;
use utoipa::ToSchema;

/// An artist or group in the shared catalog; names need not be unique.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct Artist {
    /// Stable, positive identifier assigned by SQLite.
    #[schema(minimum = 1, example = 1)]
    pub(crate) id: i64,
    /// Display name with leading and trailing whitespace removed.
    #[schema(min_length = 1, max_length = 255, example = "Massive Attack")]
    pub(crate) name: String,
}

/// Validated display name, measured in Unicode scalar values.
#[derive(Debug)]
pub(crate) struct ArtistName(String);

/// A name is blank, exceeds 255 characters, or contains control characters.
#[derive(Debug, thiserror::Error)]
#[error("Artist name must be 1–255 characters without control characters")]
pub(crate) struct InvalidArtistName;

impl ArtistName {
    /// Trim surrounding whitespace and validate before storing the name.
    pub(crate) fn parse(name: &str) -> Result<Self, InvalidArtistName> {
        let name = name.trim();
        if name.is_empty()
            || name.chars().count() > 255
            || name.chars().any(char::is_control)
        {
            return Err(InvalidArtistName);
        }
        Ok(Self(name.to_owned()))
    }

    /// Borrow the normalized name for storage without another allocation.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    //! Display-name normalization and Unicode boundaries.

    use super::*;

    /// Surrounding Unicode whitespace is removed without changing the name.
    #[test]
    fn trims_surrounding_whitespace() {
        let name = ArtistName::parse("\u{2003} Кино \n").unwrap();
        assert_eq!(name.as_str(), "Кино");
    }

    /// Limits count Unicode characters, not UTF-8 bytes.
    #[test]
    fn validates_name_length_and_controls() {
        assert!(ArtistName::parse(&"я".repeat(255)).is_ok());
        for name in [
            "".into(),
            "  ".into(),
            "a\0b".into(),
            "a\nb".into(),
            "я".repeat(256),
        ] {
            assert!(ArtistName::parse(&name).is_err(), "{name:?}");
        }
    }
}
