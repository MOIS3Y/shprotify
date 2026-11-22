//! Personal taste, explainable ranking and candidates outside the library.

pub(crate) mod ranking;

use crate::library::{enrichment::mbid, reactions::Reaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

/// Maximum occurrences stored in one generated discovery feed.
pub(crate) const MAX_ITEMS: usize = 300;

/// Candidate identity namespace; release is a specific edition.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CandidateKind {
    /// A musical recording independent of its file or release.
    Recording,
    /// An album/single/EP release, recommended as a whole.
    Release,
    /// A public external collection that can be opened as a separate feed.
    Playlist,
}

impl CandidateKind {
    /// Stable namespace for SQL identity matching.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::Release => "release",
            Self::Playlist => "playlist",
        }
    }
}

/// One credited artist, preserved without guessing identity from display name.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub(crate) struct Credit {
    /// Display name, including Cyrillic and other Unicode scripts.
    pub(crate) name: String,
    /// Optional confirmed provider-supplied MusicBrainz artist UUID.
    pub(crate) mbid: Option<String>,
}

/// Normalized discovery data; never contains an audio download URL or API key.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub(crate) struct CandidateData {
    /// Recording or release.
    pub(crate) kind: CandidateKind,
    /// Display title.
    pub(crate) title: String,
    /// Ordered display credits; not split on punctuation.
    pub(crate) artists: Vec<Credit>,
    /// Optional external identity; not required to retain a candidate.
    pub(crate) mbid: Option<String>,
    /// Origin: local, lastfm, listenbrainz or musicbrainz.
    pub(crate) provider: String,
    /// Stable provider identity, retained for future source resolution.
    pub(crate) external_id: String,
    /// Optional public information page, never fetched as an arbitrary URL.
    pub(crate) url: Option<String>,
    /// Known release date, otherwise absent.
    pub(crate) release_date: Option<String>,
}

impl CandidateData {
    /// Reject malformed upstream data before persisting public-facing cards.
    pub(crate) fn validate(&mut self) -> Result<(), &'static str> {
        self.title = text(&self.title, 255)?;
        if self.artists.len() > 100 {
            return Err("Too many artist credits");
        }
        for credit in &mut self.artists {
            credit.name = text(&credit.name, 255)?;
            credit.mbid = credit.mbid.as_deref().map(mbid).transpose()?;
        }
        self.mbid = self.mbid.as_deref().map(mbid).transpose()?;
        if self.external_id.is_empty() || self.external_id.len() > 4096 {
            return Err("Invalid provider identity");
        }
        if let Some(url) = &self.url {
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| "Invalid provider page")?;
            if !["http", "https"].contains(&parsed.scheme())
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || url.len() > 4096
            {
                return Err("Invalid provider page");
            }
        }
        Ok(())
    }

    /// Deduplicate by verified MBID, otherwise preserve provider identity.
    pub(crate) fn identity(&self) -> String {
        self.mbid.as_ref().map_or_else(
            || format!("{}:{}", self.provider, self.external_id),
            |id| format!("{}:{id}", self.kind.as_str()),
        )
    }
}

/// Bounded plain text; normalization preserves spelling and script.
pub(crate) fn text(value: &str, max: usize) -> Result<String, &'static str> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > max
        || value.chars().any(char::is_control)
    {
        return Err("Text is empty, too long or contains control characters");
    }
    Ok(value.to_owned())
}

/// Deterministic key for canonical requests; secret keys never enter input.
pub(crate) fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Explicit collection request; unrelated options are rejected by serde.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum FeedRequest {
    /// Blend local discoveries with external candidates from personal seeds.
    Personal {},
    /// Return favorite recordings not played fully in the last 30 days.
    Rediscover {},
    /// Global Last.fm track chart.
    Chart {},
    /// Last.fm chart for an ISO country name, independent of track language.
    Country { country: String },
    /// Last.fm thematic selection for a tag.
    Tag { tag: String },
    /// Similar tracks using artist and recording names.
    SimilarTrack { artist: String, track: String },
    /// Tracks from related artists, using Last.fm's similarity graph.
    SimilarArtist { artist: String },
    /// Literal Last.fm track search, optionally narrowed to an artist.
    Search {
        query: String,
        artist: Option<String>,
    },
    /// Recent editions from MusicBrainz for personal artist MBID seeds.
    NewReleases {},
    /// Search public ListenBrainz playlists by title or description.
    PlaylistSearch { query: String },
    /// Fetch a public ListenBrainz playlist by UUID.
    Playlist { playlist_id: String },
}

impl FeedRequest {
    /// Validate and trim before deriving cache/admission keys.
    pub(crate) fn validate(&mut self) -> Result<(), &'static str> {
        match self {
            Self::PlaylistSearch { query } => {
                *query = text(query, 255)?;
                if query.chars().count() < 3 {
                    return Err(
                        "Playlist search requires at least 3 characters",
                    );
                }
            }
            Self::Country { country } => *country = text(country, 100)?,
            Self::Tag { tag } => *tag = text(tag, 100)?,
            Self::SimilarTrack { artist, track } => {
                *artist = text(artist, 255)?;
                *track = text(track, 255)?;
            }
            Self::SimilarArtist { artist } => *artist = text(artist, 255)?,
            Self::Search { query, artist } => {
                *query = text(query, 255)?;
                *artist =
                    artist.as_deref().map(|s| text(s, 255)).transpose()?;
            }
            Self::Playlist { playlist_id } => {
                *playlist_id = mbid(playlist_id)?
            }
            _ => (),
        }
        Ok(())
    }

    /// Generated recommendation feeds are reranked; source charts keep order.
    pub(crate) fn personalized(&self) -> bool {
        matches!(
            self,
            Self::Personal {} | Self::Rediscover {} | Self::NewReleases {}
        )
    }
}

/// Stored candidate and current local match, personal decision and
/// explanation.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Recommendation {
    /// Stable discovery identity; remains valid after a source refresh.
    pub(crate) id: i64,
    /// Candidate metadata without modifying the shared catalog.
    pub(crate) candidate: CandidateData,
    /// Matched local Recording/Release ID; null means unresolved externally.
    pub(crate) local_id: Option<i64>,
    /// Exact MBID or unique exact title/credit match; null when unresolved.
    pub(crate) match_method: Option<String>,
    /// Recording has a registered file, or every known release track has one.
    /// Physical availability is checked only when streaming.
    pub(crate) has_registered_audio: bool,
    /// Current effective local reaction, falling back to discovery feedback.
    pub(crate) reaction: Option<Reaction>,
    /// Versioned discovery feedback, independent of local reaction edits.
    pub(crate) feedback: Feedback,
    /// Local ordering score, not a probability or global recording rating.
    pub(crate) score: f64,
    /// Human-readable reasons and provider/seed provenance.
    pub(crate) reasons: Vec<String>,
}

/// Complete candidate feedback; null reaction removes that explicit
/// assessment.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Feedback {
    /// Zero until the first decision; stale edits receive 409.
    pub(crate) version: i64,
    /// Like/dislike or null; independent of hidden and wanted flags.
    pub(crate) reaction: Option<Reaction>,
    /// Exclude from generated suggestions until explicitly cleared.
    pub(crate) hidden: bool,
    /// Retain in the personal acquisition wishlist before downloads exist.
    pub(crate) wanted: bool,
}

impl Default for Feedback {
    /// Reading an untouched candidate never creates a preference row.
    fn default() -> Self {
        Self {
            version: 0,
            reaction: None,
            hidden: false,
            wanted: false,
        }
    }
}

/// Explicit onboarding and topical preferences, separate from inferred taste.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    /// Zero initially; optimistic precondition shared by all preference
    /// fields.
    pub(crate) version: i64,
    /// Up to 20 distinct existing local artist IDs followed by this account.
    pub(crate) artist_ids: Vec<i64>,
    /// Up to 20 distinct Last.fm tags, preserving readable user spelling.
    pub(crate) tags: Vec<String>,
}

/// Inferred artist affinity used for seeds and local/external ranking.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct ArtistAffinity {
    /// Display name from a local credit or explicit external like.
    pub(crate) name: String,
    /// MusicBrainz UUID when known.
    pub(crate) mbid: Option<String>,
    /// Bounded positive affinity from follows, likes and full listens.
    pub(crate) weight: f64,
}

/// Track-level positive seed; no complete listening history is sent upstream.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub(crate) struct TrackSeed {
    /// Recording title.
    pub(crate) title: String,
    /// One artist credit, preserved as written.
    pub(crate) artist: String,
    /// Bounded affinity.
    pub(crate) weight: f64,
}

/// Local personal profile, including explicit onboarding settings.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Profile {
    /// Explicit preferences with their own version.
    pub(crate) preferences: Preferences,
    /// Up to 20 artists in decreasing affinity order.
    pub(crate) artists: Vec<ArtistAffinity>,
    /// Up to ten positive recording seeds.
    pub(crate) tracks: Vec<TrackSeed>,
}

/// One generated entry before persistent identity assignment.
pub(crate) struct Suggestion {
    /// Candidate information from a local or external source.
    pub(crate) candidate: CandidateData,
    /// Bounded source relevance; personalized ranking adds local affinity.
    pub(crate) weight: f64,
    /// Reasons retained with this feed occurrence, not the global candidate.
    pub(crate) reasons: Vec<String>,
}

/// Refresh output commits atomically; warnings allow partial personal results.
pub(crate) struct GeneratedFeed {
    /// Provider title or generated collection name.
    pub(crate) title: String,
    /// Original public collection page if one exists.
    pub(crate) source_url: Option<String>,
    /// Bounded candidate occurrences.
    pub(crate) items: Vec<Suggestion>,
    /// Safe degradation messages; no raw provider errors or API secrets.
    pub(crate) warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    //! Request shape, Unicode text and provider identity contracts.
    use super::*;

    /// UUID is optional; provider identity is sufficient for stable retention.
    #[test]
    fn keeps_external_candidates_without_musicbrainz_and_validates_requests() {
        let mut item = crate::db::discovery::tests::suggestion(
            "lastfm",
            "opaque",
            "  Неизвестный трек  ",
            None,
        )
        .candidate;
        item.validate().unwrap();
        assert_eq!(item.title, "Неизвестный трек");
        assert_eq!(item.identity(), "lastfm:opaque");
        item.mbid = Some(crate::db::discovery::tests::MBID.into());
        assert!(item.identity().starts_with("recording:"));
        assert!(
            serde_json::from_str::<FeedRequest>(
                r#"{"kind":"chart","extra":true}"#
            )
            .is_err()
        );
        let mut invalid = FeedRequest::Playlist {
            playlist_id: "../bad".into(),
        };
        assert!(invalid.validate().is_err());
        let mut valid = FeedRequest::SimilarTrack {
            artist: " Кино ".into(),
            track: " Группа крови ".into(),
        };
        valid.validate().unwrap();
        assert!(matches!(valid,FeedRequest::SimilarTrack {artist,..}
            if artist=="Кино"));
    }
}
