//! Compact client read models; registered availability is not a disk probe.

use super::{Artist, audio_files::AudioProperties, releases::ReleaseKind};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Sort keys supported by the individual catalog collection.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Sort {
    /// Stable creation/identifier order; default for existing clients.
    #[default]
    Id,
    /// Recording/release display title, SQLite NOCASE order.
    Title,
    /// Artist display name, SQLite NOCASE order.
    Name,
    /// Representative duration, unknown values last in either direction.
    Duration,
}
impl Sort {
    /// Bind a controlled label; it is never interpolated into SQL.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Title => "title",
            Self::Name => "name",
            Self::Duration => "duration",
        }
    }
}

/// Direction uses an ID tie-breaker to keep identical titles deterministic.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Order {
    /// Increasing values; default.
    #[default]
    Asc,
    /// Decreasing values.
    Desc,
}
impl Order {
    /// SQL binds use a boolean instead of a dynamic ordering expression.
    pub(crate) fn descending(self) -> bool {
        matches!(self, Self::Desc)
    }
}

/// Validate bounded pagination and positive optional identity filters.
fn validate(
    limit: i64,
    offset: i64,
    ids: &[Option<i64>],
) -> Result<(), &'static str> {
    if !(1..=100).contains(&limit)
        || offset < 0
        || ids.iter().flatten().any(|id| *id <= 0)
    {
        return Err(
            "Use limit 1–100, nonnegative offset and positive filter IDs",
        );
    }
    Ok(())
}

/// Recording filters are combined; all matching rows precede pagination.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub(crate) struct RecordingQuery {
    /// Page size, 1–100, default 50.
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub(crate) limit: i64,
    /// Number of matched recordings skipped.
    #[param(minimum = 0, default = 0)]
    pub(crate) offset: i64,
    /// Filter credited performer.
    pub(crate) artist_id: Option<i64>,
    /// Filter recordings occurring in a release; duplicates remain one
    /// recording.
    pub(crate) release_id: Option<i64>,
    /// Filter registered, non-removing audio; no filesystem availability
    /// claim.
    pub(crate) has_audio: Option<bool>,
    /// id, title or duration; default id.
    pub(crate) sort: Sort,
    /// asc or desc, default asc.
    pub(crate) order: Order,
}
impl Default for RecordingQuery {
    /// Preserve the former unfiltered first fifty records.
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            artist_id: None,
            release_id: None,
            has_audio: None,
            sort: Sort::Id,
            order: Order::Asc,
        }
    }
}
impl RecordingQuery {
    /// Reject unsupported sort/filter input before SQL execution.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        validate(self.limit, self.offset, &[self.artist_id, self.release_id])?;
        if matches!(self.sort, Sort::Name) {
            return Err("Use id, title or duration sort");
        }
        Ok(())
    }
}

/// Release filters support artist discographies and recording appearances.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub(crate) struct ReleaseQuery {
    /// Page size, 1–100, default 50.
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub(crate) limit: i64,
    /// Number of matched releases skipped.
    #[param(minimum = 0, default = 0)]
    pub(crate) offset: i64,
    /// Credited release artist; compilation track artists do not imply this
    /// credit.
    pub(crate) artist_id: Option<i64>,
    /// Releases containing a recording.
    pub(crate) recording_id: Option<i64>,
    /// Optional release classification.
    pub(crate) kind: Option<ReleaseKind>,
    /// Whether at least one known track has registered audio.
    pub(crate) has_audio: Option<bool>,
    /// id, title or duration; default id.
    pub(crate) sort: Sort,
    /// asc or desc, default asc.
    pub(crate) order: Order,
}
impl Default for ReleaseQuery {
    /// Default remains the first fifty releases in ID order.
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            artist_id: None,
            recording_id: None,
            kind: None,
            has_audio: None,
            sort: Sort::Id,
            order: Order::Asc,
        }
    }
}
impl ReleaseQuery {
    /// Validate identity and ordering inputs without changing their meaning.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        validate(
            self.limit,
            self.offset,
            &[self.artist_id, self.recording_id],
        )?;
        if matches!(self.sort, Sort::Name) {
            return Err("Use id, title or duration sort");
        }
        Ok(())
    }
}

/// Artist pages retain bounded pagination and add deterministic name sorting.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub(crate) struct ArtistQuery {
    /// Page size, 1–100, default 50.
    #[param(minimum = 1, maximum = 100, default = 50)]
    pub(crate) limit: i64,
    /// Number of artists skipped.
    #[param(minimum = 0, default = 0)]
    pub(crate) offset: i64,
    /// id or name, default id.
    pub(crate) sort: Sort,
    /// asc or desc, default asc.
    pub(crate) order: Order,
}
impl Default for ArtistQuery {
    /// Preserve ID ordering unless the client explicitly chooses another key.
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            sort: Sort::Id,
            order: Order::Asc,
        }
    }
}
impl ArtistQuery {
    /// Reject sort keys belonging to other catalog entities.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        validate(self.limit, self.offset, &[])?;
        if !matches!(self.sort, Sort::Id | Sort::Name) {
            return Err("Use id or name sort");
        }
        Ok(())
    }
}

/// Path-free registered media information suitable for rendering a whole page.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub(crate) struct FileSummary {
    /// Stable physical-file catalog ID.
    pub(crate) id: i64,
    /// Detected or registered container/codec label.
    pub(crate) format: String,
    /// Current registered properties; unknown fields remain null.
    #[serde(flatten)]
    pub(crate) properties: AudioProperties,
    /// Relative Bearer-authenticated media URL, without a credential in it.
    pub(crate) stream_url: String,
}

/// Recording identity with ordered display credits and all available formats.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct RecordingSummary {
    /// Stable recording ID, independent of its physical files.
    pub(crate) id: i64,
    /// Current curated title.
    pub(crate) title: String,
    /// Existing ordered artist-ID contract.
    #[sqlx(json)]
    pub(crate) artist_ids: Vec<i64>,
    /// Ordered display credits, avoiding a lookup per artist.
    #[sqlx(json)]
    pub(crate) artists: Vec<Artist>,
    /// Path-free file summaries; deleting files are excluded.
    #[sqlx(json)]
    pub(crate) files: Vec<FileSummary>,
    /// Count of registered, non-removing files; not a physical disk probe.
    pub(crate) audio_file_count: i64,
    /// Duration of the earliest registered file with a known duration.
    pub(crate) duration_ms: Option<i64>,
    /// Recording cover, otherwise the first associated release cover by ID.
    pub(crate) artwork_url: Option<String>,
    /// True when audio_file_count is positive.
    pub(crate) has_audio: bool,
}

/// Discography summary with track/audio counts and ready display credits.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct ReleaseSummary {
    /// Stable release ID.
    pub(crate) id: i64,
    /// Curated release title.
    pub(crate) title: String,
    /// Album/single/EP/compilation/other classification.
    pub(crate) kind: ReleaseKind,
    /// Existing ordered artist-ID contract.
    #[sqlx(json)]
    pub(crate) artist_ids: Vec<i64>,
    /// Ordered display credits.
    #[sqlx(json)]
    pub(crate) artists: Vec<Artist>,
    /// Known release-track occurrences, including repeated recordings.
    pub(crate) track_count: i64,
    /// Occurrences with at least one registered, non-removing audio file.
    pub(crate) available_track_count: i64,
    /// Sum only when all known tracks have a duration; otherwise null.
    pub(crate) duration_ms: Option<i64>,
    /// Direct release artwork URL, if present.
    pub(crate) artwork_url: Option<String>,
    /// At least one known track has audio.
    pub(crate) has_audio: bool,
    /// All known tracks have audio; no claim about an external full tracklist.
    pub(crate) is_complete: bool,
}

/// Artist cards expose counts without requiring a full catalog download.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct ArtistSummary {
    /// Stable artist ID.
    pub(crate) id: i64,
    /// Current display name.
    pub(crate) name: String,
    /// Recordings crediting this artist.
    pub(crate) recording_count: i64,
    /// Releases crediting this artist.
    pub(crate) release_count: i64,
    /// Direct artist artwork URL, if present.
    pub(crate) artwork_url: Option<String>,
}

/// Counts of the common catalog, without personal data or filesystem paths.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct LibraryStats {
    /// Registered artists.
    pub(crate) artists: i64,
    /// Registered releases.
    pub(crate) releases: i64,
    /// Known recordings, including ones without audio.
    pub(crate) recordings: i64,
    /// Recordings with non-removing registered audio.
    pub(crate) recordings_with_audio: i64,
    /// Non-removing registered physical files.
    pub(crate) audio_files: i64,
}
