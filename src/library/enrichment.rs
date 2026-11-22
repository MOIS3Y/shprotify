//! Versioned catalog corrections and proposals independent of transport.

use super::releases::ReleaseKind;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Catalog identity namespace; release means an edition, not a release group.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EntityKind {
    /// Performer/group identity.
    Artist,
    /// Specific release edition.
    Release,
    /// Recording independent of files and releases.
    Recording,
}

impl EntityKind {
    /// Stable SQL and external API namespace.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Release => "release",
            Self::Recording => "recording",
        }
    }
}

/// Complete editable metadata plus revision and artwork provenance.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Metadata {
    /// Shared catalog namespace.
    pub(crate) entity_type: EntityKind,
    /// Stable local identity; edits never merge/delete catalog entities.
    pub(crate) entity_id: i64,
    /// Optimistic precondition shared by metadata and artwork changes.
    pub(crate) version: i64,
    /// Artist display name or recording/release title.
    pub(crate) name: String,
    /// Ordered local credits; empty for artist entities.
    pub(crate) artist_ids: Vec<i64>,
    /// Present only for releases.
    pub(crate) release_kind: Option<ReleaseKind>,
    /// Confirmed MusicBrainz identity, if known.
    pub(crate) mbid: Option<String>,
    /// Last metadata edit origin; existing means unaudited baseline.
    pub(crate) source: String,
    /// Current cover digest, or null when there is no cover.
    pub(crate) artwork_digest: Option<String>,
    /// Artwork origin, independent of text metadata.
    pub(crate) artwork_source: Option<String>,
}

/// Validated full correction; explicit null mbid removes the identity link.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Correction {
    /// Last observed metadata version; must be positive.
    pub(crate) version: i64,
    /// Display name/title, trimmed to 1–255 characters without controls.
    pub(crate) name: String,
    /// Complete ordered local artist credits, at most 100 distinct IDs.
    pub(crate) artist_ids: Vec<i64>,
    /// Required for a release; omitted/null for artists and recordings.
    pub(crate) release_kind: Option<ReleaseKind>,
    /// MusicBrainz UUID, or omitted/null to clear the link.
    pub(crate) mbid: Option<String>,
}

impl Correction {
    /// Apply the same validation to manual and external metadata.
    pub(crate) fn validate(
        &mut self,
        kind: EntityKind,
    ) -> Result<(), &'static str> {
        if self.version <= 0 {
            return Err("version must be positive");
        }
        self.name = super::validate_title(&self.name)?.to_owned();
        super::validate_artist_ids(&self.artist_ids)?;
        if kind == EntityKind::Artist && !self.artist_ids.is_empty() {
            return Err("Artists cannot have artist_ids");
        }
        if (kind == EntityKind::Release) != self.release_kind.is_some() {
            return Err("release_kind is required only for releases");
        }
        self.mbid = self.mbid.as_deref().map(mbid).transpose()?;
        Ok(())
    }
}

/// Canonicalize a UUID-shaped MusicBrainz ID; never accept a URL or list.
pub(crate) fn mbid(value: &str) -> Result<String, &'static str> {
    let value = value.trim();
    if value.len() != 36
        || !value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        || value.bytes().all(|b| b == b'0' || b == b'-')
    {
        return Err("mbid must be a nonzero UUID");
    }
    Ok(value.to_ascii_lowercase())
}

/// External artist identity; names alone never merge local artists.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub(crate) struct ArtistCredit {
    /// MusicBrainz artist UUID.
    pub(crate) mbid: String,
    /// External display name.
    pub(crate) name: String,
}

/// Persisted suggestion. Nothing is applied automatically, including artwork.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub(crate) struct Proposal {
    /// musicbrainz, acoustid, embedded or cover_art_archive.
    pub(crate) source: String,
    /// Candidate identity; absent for artwork-only suggestions.
    pub(crate) mbid: Option<String>,
    /// Candidate name/title; absent for artwork-only suggestions.
    pub(crate) name: Option<String>,
    /// Provider disambiguation/date/country to distinguish similar results.
    pub(crate) description: Option<String>,
    /// External credit order; imported only on explicit confirmation.
    pub(crate) artists: Vec<ArtistCredit>,
    /// Provider relevance score, not a probability of correctness.
    pub(crate) score: Option<f64>,
    /// Artwork digest and MIME are present for artwork-only suggestions.
    pub(crate) artwork_digest: Option<String>,
    /// Validated media type, independent of file suffix or response headers.
    pub(crate) artwork_mime: Option<String>,
}

/// Explicit application policy; identity is linked for metadata candidates.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApplyCandidate {
    /// Current entity version; stale proposals cannot silently overwrite it.
    pub(crate) version: i64,
    /// Replace name/title using this candidate; false preserves manual text.
    pub(crate) name: bool,
    /// Replace credits using exact external IDs; false preserves local
    /// credits.
    pub(crate) credits: bool,
}

#[cfg(test)]
mod tests {
    //! Identity canonicalization and type-specific correction contracts.
    use super::*;

    /// Only UUID-shaped nonzero identifiers may become external identities.
    #[test]
    fn validates_external_ids_and_correction_shape() {
        assert_eq!(
            mbid(" ABCDEF12-1234-1234-1234-123456789ABC ").unwrap(),
            "abcdef12-1234-1234-1234-123456789abc"
        );
        for id in [
            "",
            "00000000-0000-0000-0000-000000000000",
            "https://musicbrainz.org/recording/123",
            "not-an-id",
        ] {
            assert!(mbid(id).is_err());
        }
        let mut input = Correction {
            version: 1,
            name: " Name ".into(),
            artist_ids: vec![],
            release_kind: None,
            mbid: None,
        };
        input.validate(EntityKind::Artist).unwrap();
        assert_eq!(input.name, "Name");
        assert!(input.validate(EntityKind::Release).is_err());
        input.release_kind = Some(ReleaseKind::Album);
        input.validate(EntityKind::Release).unwrap();
        assert!(input.validate(EntityKind::Recording).is_err());
        input.release_kind = None;
        input.artist_ids = vec![1, 1];
        assert!(input.validate(EntityKind::Recording).is_err());
    }
}

/// Association namespace for correcting an incorrectly matched source/track.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinkKind {
    /// Registered AudioFile; source bytes and observed tags stay untouched.
    File,
    /// ReleaseTrack occurrence; disc/position and release stay untouched.
    ReleaseTrack,
}

/// Compare-and-set association edit; this never moves personal history/likes.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkCorrection {
    /// Recording currently associated with this file or release occurrence.
    pub(crate) expected_recording_id: i64,
    /// Correct existing recording identity.
    pub(crate) recording_id: i64,
}
