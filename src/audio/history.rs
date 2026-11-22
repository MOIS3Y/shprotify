//! Client-reported listening facts and stable full/short classification.
//!
//! A report describes one finished/stopped playback, not a media download.
//! Listen time excludes pauses and seeks, and is capped at known duration.

use serde::Serialize;
use utoipa::ToSchema;

/// Server classification, preserved with each accepted event.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type, ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub(crate) enum PlaybackKind {
    /// At least half the known duration or four minutes of listening.
    Full,
    /// Less than the qualifying threshold; retained in personal history.
    Short,
}

/// Immutable listening facts validated before any database mutation.
#[derive(Debug)]
pub(crate) struct NewPlayback {
    /// Opaque client idempotency key, unique within the user's history.
    pub(crate) event_id: String,
    /// Shared recording, independent of its file format or release position.
    pub(crate) recording_id: i64,
    /// Client-reported playback start, in Unix seconds.
    pub(crate) started_at: i64,
    /// Actual cumulative media time played, excluding pauses and seeks.
    pub(crate) listened_ms: i64,
    /// Duration known to the player, or None when unavailable.
    pub(crate) duration_ms: Option<i64>,
    /// Classification computed from the validated listening facts.
    pub(crate) kind: PlaybackKind,
}

impl NewPlayback {
    /// Validate reported facts without consulting files or trusting a user ID.
    pub(crate) fn new(
        event_id: String,
        recording_id: i64,
        started_at: i64,
        listened_ms: i64,
        duration_ms: Option<i64>,
    ) -> Result<Self, &'static str> {
        if event_id.is_empty()
            || event_id.len() > 128
            || !event_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err("event_id must be 1–128 ASCII letters, digits or ._-");
        }
        if recording_id <= 0 {
            return Err("recording_id must be positive");
        }
        if started_at < 0 {
            return Err("started_at must be nonnegative Unix seconds");
        }
        if listened_ms <= 0 {
            return Err("listened_ms must be positive");
        }
        if duration_ms.is_some_and(|d| d <= 0 || listened_ms > d) {
            return Err(
                "duration_ms must be positive and at least listened_ms",
            );
        }
        let threshold = duration_ms.map_or(240_000, |duration| {
            (duration / 2 + duration % 2).min(240_000)
        });
        let kind = if listened_ms >= threshold {
            PlaybackKind::Full
        } else {
            PlaybackKind::Short
        };
        Ok(Self {
            event_id,
            recording_id,
            started_at,
            listened_ms,
            duration_ms,
            kind,
        })
    }

    /// Retrying the same key must preserve all originally reported facts.
    pub(crate) fn matches(&self, entry: &PlaybackEvent) -> bool {
        self.recording_id == entry.recording_id
            && self.started_at == entry.started_at
            && self.listened_ms == entry.listened_ms
            && self.duration_ms == entry.duration_ms
    }
}

/// A user-scoped history item, with its current shared catalog title.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct PlaybackEvent {
    /// Stable history identifier; another user's ID grants no access.
    pub(crate) id: i64,
    /// Original client idempotency key.
    pub(crate) event_id: String,
    /// Shared recording identity.
    pub(crate) recording_id: i64,
    /// Current catalog title, including manual corrections.
    pub(crate) title: String,
    /// Reported playback start in Unix seconds, used to order history.
    pub(crate) started_at: i64,
    /// Accepted media time played, in milliseconds.
    pub(crate) listened_ms: i64,
    /// Accepted duration, or null when unknown.
    pub(crate) duration_ms: Option<i64>,
    /// Classification at acceptance, never supplied by the client.
    pub(crate) kind: PlaybackKind,
    /// Server receipt time in UTC ISO 8601 with millisecond precision.
    pub(crate) received_at: String,
}

#[cfg(test)]
mod tests {
    //! Validation, half-duration rounding and the four-minute fallback.

    use super::*;

    /// Thresholds use actual listened time rather than a final seek position.
    #[test]
    fn classifies_exact_thresholds_short_tracks_and_unknown_duration() {
        for (duration, listened, kind) in [
            (Some(180_000), 89_999, PlaybackKind::Short),
            (Some(180_000), 90_000, PlaybackKind::Full),
            (Some(1), 1, PlaybackKind::Full),
            (Some(3), 1, PlaybackKind::Short),
            (Some(3), 2, PlaybackKind::Full),
            (Some(900_000), 239_999, PlaybackKind::Short),
            (Some(900_000), 240_000, PlaybackKind::Full),
            (None, 239_999, PlaybackKind::Short),
            (None, 240_000, PlaybackKind::Full),
            (Some(i64::MAX), 240_000, PlaybackKind::Full),
        ] {
            let event =
                NewPlayback::new("event".into(), 1, 0, listened, duration)
                    .unwrap();
            assert_eq!(event.kind, kind);
        }
    }

    /// IDs stay opaque and facts must be positive and internally consistent.
    #[test]
    fn rejects_invalid_identifiers_times_and_durations() {
        for id in ["", " a", "a ", "../x", "имя", &"a".repeat(129)] {
            assert!(NewPlayback::new(id.into(), 1, 0, 1, None).is_err());
        }
        for (recording, start, listened, duration) in [
            (0, 0, 1, None),
            (1, -1, 1, None),
            (1, 0, 0, None),
            (1, 0, -1, None),
            (1, 0, 1, Some(0)),
            (1, 0, 1, Some(-1)),
            (1, 0, 11, Some(10)),
        ] {
            assert!(
                NewPlayback::new(
                    "valid".into(),
                    recording,
                    start,
                    listened,
                    duration
                )
                .is_err()
            );
        }
        assert!(NewPlayback::new("A.b_c-123".into(), 1, 0, 1, None).is_ok());
    }
}
