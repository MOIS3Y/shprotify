//! Conservative source-tag validation for directory-local catalog matching.
//!
//! Values are trimmed but never split, case-folded, or inferred from paths.
//! Album-artist and recording-artist credits remain separate. Missing track
//! numbers do not imply positions; only a missing disc number defaults to one.

use super::{metadata::AudioTags, validate_title};

/// Validated, optional metadata for creating associations on a new recording.
#[derive(Debug)]
pub(crate) struct MatchingTags {
    /// Explicit performer credit, preserved as one name even with separators.
    pub(crate) artist: Option<String>,
    /// Album title; absent means no release or release track is inferred.
    pub(crate) album: Option<String>,
    /// Explicit release-level credit; never inferred from the performer.
    pub(crate) album_artist: Option<String>,
    /// Positive track position; absent means no release track is inferred.
    pub(crate) track: Option<u32>,
    /// Positive disc number, defaulting to one only when absent.
    pub(crate) disc: u32,
}

impl MatchingTags {
    /// Validate matching fields without altering the original source tags.
    ///
    /// Invalid optional metadata skips association creation for this file;
    /// callers may still store its recording, file, and original observations.
    pub(crate) fn parse(tags: &AudioTags) -> Result<Self, &'static str> {
        if tags.track_number == Some(0) || tags.disc_number == Some(0) {
            return Err("Source track and disc numbers must be positive");
        }
        Ok(Self {
            artist: optional_text(
                tags.artist.as_deref(),
                "Source artist exceeds 255 characters or contains controls",
            )?,
            album: optional_text(
                tags.album.as_deref(),
                "Source album exceeds 255 characters or contains controls",
            )?,
            album_artist: optional_text(
                tags.album_artist.as_deref(),
                "Source album artist exceeds 255 chars or contains controls",
            )?,
            track: tags.track_number,
            disc: tags.disc_number.unwrap_or(1),
        })
    }
}

/// Treat blank tags as absent and validate nonblank names/titles identically.
fn optional_text(
    text: Option<&str>,
    message: &'static str,
) -> Result<Option<String>, &'static str> {
    text.filter(|text| !text.trim().is_empty())
        .map(|text| {
            validate_title(text).map(str::to_owned).map_err(|_| message)
        })
        .transpose()
}

/// Normalize one explicit UUID-shaped recording ID; never infer it from names.
///
/// Blank values mean unknown. URLs, lists, nil IDs, and malformed values are
/// rejected; this checks syntax, not existence in an external catalog.
pub(crate) fn recording_id(
    value: Option<&str>,
) -> Result<Option<String>, &'static str> {
    let Some(value) = value.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let valid = value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        });
    if !valid || value.bytes().all(|b| b == b'0' || b == b'-') {
        return Err("Invalid source MusicBrainz recording ID");
    }
    Ok(Some(value.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    //! Missing values, exact credits, Unicode boundaries, and invalid numbers.

    use super::*;

    /// Identity syntax is exact; only whitespace and hex case are normalized.
    #[test]
    fn validates_explicit_recording_ids() {
        let id = "ebf79ba5-085e-48d2-9eb8-2d992fbf0f6d";
        assert_eq!(
            recording_id(Some(&format!(" {} ", id.to_uppercase())))
                .unwrap()
                .as_deref(),
            Some(id)
        );
        assert!(recording_id(None).unwrap().is_none());
        assert!(recording_id(Some(" ")).unwrap().is_none());
        for invalid in [
            "name",
            "00000000-0000-0000-0000-000000000000",
            "https://musicbrainz.org/recording/id",
            "ebf79ba5_085e-48d2-9eb8-2d992fbf0f6d",
        ] {
            assert!(recording_id(Some(invalid)).is_err());
        }
    }

    /// Only surrounding whitespace changes; compound credits stay intact.
    #[test]
    fn trims_values_without_splitting_or_inventing_credits() {
        let source = AudioTags {
            artist: Some(" A feat. B / C; D ".into()),
            album: Some(" Альбом ".into()),
            album_artist: Some(" \t ".into()),
            track_number: Some(3),
            ..AudioTags::default()
        };
        let tags = MatchingTags::parse(&source).unwrap();
        assert_eq!(tags.artist.as_deref(), Some("A feat. B / C; D"));
        assert_eq!(tags.album.as_deref(), Some("Альбом"));
        assert!(tags.album_artist.is_none());
        assert_eq!(tags.track, Some(3));
        assert_eq!(tags.disc, 1);
        assert_eq!(source.album.as_deref(), Some(" Альбом "));
        let empty = MatchingTags::parse(&AudioTags::default()).unwrap();
        assert!(empty.artist.is_none());
        assert!(empty.album.is_none());
        assert!(empty.track.is_none());
    }

    /// Invalid source values are reported instead of truncation or guessing.
    #[test]
    fn rejects_invalid_names_and_positions() {
        for source in [
            AudioTags {
                artist: Some("a\nb".into()),
                ..AudioTags::default()
            },
            AudioTags {
                album: Some("a".repeat(256)),
                ..AudioTags::default()
            },
            AudioTags {
                album_artist: Some("x\0y".into()),
                ..AudioTags::default()
            },
            AudioTags {
                track_number: Some(0),
                ..AudioTags::default()
            },
            AudioTags {
                disc_number: Some(0),
                ..AudioTags::default()
            },
        ] {
            assert!(MatchingTags::parse(&source).is_err());
        }
        let tags = MatchingTags::parse(&AudioTags {
            artist: Some("я".repeat(255)),
            track_number: Some(u32::MAX),
            disc_number: Some(2),
            ..AudioTags::default()
        })
        .unwrap();
        assert_eq!(tags.artist.unwrap().chars().count(), 255);
        assert_eq!(tags.disc, 2);
    }
}
