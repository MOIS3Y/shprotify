//! Audio file metadata linked to recordings, without filesystem side effects.
//!
//! Registration stores declared properties. File existence, media probing,
//! symlink resolution, and streaming belong to later scanning/audio stages.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Optional measured properties; unknown values remain null rather than zero.
#[derive(Debug, Default, Deserialize, Serialize, sqlx::FromRow, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AudioProperties {
    /// File size in bytes, when known.
    #[schema(minimum = 1)]
    pub(crate) size_bytes: Option<i64>,
    /// Audio duration in milliseconds, when known.
    #[schema(minimum = 1)]
    pub(crate) duration_ms: Option<i64>,
    /// Audio sampling frequency in hertz.
    #[schema(minimum = 1)]
    pub(crate) sample_rate_hz: Option<i64>,
    /// Number of audio channels.
    #[schema(minimum = 1)]
    pub(crate) channels: Option<i64>,
    /// PCM sample bit depth, when applicable and known.
    #[schema(minimum = 1)]
    pub(crate) bit_depth: Option<i64>,
    /// Audio bitrate in bits per second, when known.
    #[schema(minimum = 1)]
    pub(crate) bitrate_bps: Option<i64>,
}

impl AudioProperties {
    /// Reject nonpositive values and preserve unknown properties.
    fn validate(&self) -> Result<(), &'static str> {
        if [
            self.size_bytes,
            self.duration_ms,
            self.sample_rate_hz,
            self.channels,
            self.bit_depth,
            self.bitrate_bps,
        ]
        .into_iter()
        .flatten()
        .any(|value| value <= 0)
        {
            return Err("Known audio properties must be positive integers");
        }
        Ok(())
    }
}

/// One registered physical file; several files may reference one recording.
#[derive(Debug, Serialize, sqlx::FromRow, ToSchema)]
pub(crate) struct AudioFile {
    /// Stable positive identifier of this catalog file.
    #[schema(minimum = 1)]
    pub(crate) id: i64,
    /// Shared recording represented by this file.
    #[schema(minimum = 1)]
    pub(crate) recording_id: i64,
    /// Absolute UTF-8 path, normalized lexically without resolving symlinks.
    pub(crate) path: String,
    /// Lowercase declared format label, such as flac, mp3, ogg, or m4a.
    pub(crate) format: String,
    /// Declared properties; values are not verified against file contents yet.
    #[sqlx(flatten)]
    pub(crate) properties: AudioProperties,
}

/// Validated file metadata for registration without reading or changing files.
#[derive(Debug)]
pub(crate) struct NewAudioFile {
    /// Normalized absolute path; unique across the instance catalog.
    pub(crate) path: String,
    /// Normalized format label, independent of the file extension.
    pub(crate) format: String,
    /// Optional declared audio and file properties.
    pub(crate) properties: AudioProperties,
}

impl NewAudioFile {
    /// Validate path, format, and properties without accessing the filesystem.
    pub(crate) fn new(
        path: &str,
        format: &str,
        properties: AudioProperties,
    ) -> Result<Self, &'static str> {
        let path = normalize_path(path)?;
        let format = format.trim().to_ascii_lowercase();
        if format.is_empty()
            || format.len() > 32
            || !format.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'-'
                    || byte == b'_'
            })
        {
            return Err("Format must be a 1–32 character ASCII format label");
        }
        properties.validate()?;
        Ok(Self {
            path,
            format,
            properties,
        })
    }
}

/// Normalize separators and dot components, preserving spaces in filenames.
///
/// Parent traversal and directory-like endings are rejected. This identity
/// is lexical: symlink and hard-link aliases are handled by future scanners.
fn normalize_path(value: &str) -> Result<String, &'static str> {
    let path = Path::new(value);
    if !path.is_absolute()
        || value.len() > 4096
        || value.chars().any(char::is_control)
        || value.ends_with('/')
        || value.ends_with("/.")
        || path.components().any(|part| part == Component::ParentDir)
        || path.file_name().is_none()
    {
        return Err(
            "Path must be an absolute file path without parent traversal",
        );
    }
    let normalized: PathBuf = path.components().collect();
    Ok(normalized.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    //! Lexical identity and optional metadata boundaries, without file I/O.

    use super::*;

    /// Path aliases normalize while filename spaces remain unchanged.
    #[test]
    fn normalizes_labels_and_paths_without_trimming_filenames() {
        let file = NewAudioFile::new(
            "/music//./Кино/song .flac ",
            " FLAC ",
            AudioProperties::default(),
        )
        .unwrap();
        assert_eq!(file.path, "/music/Кино/song .flac ");
        assert_eq!(file.format, "flac");
        assert!(file.properties.duration_ms.is_none());
    }

    /// Relative, ambiguous, traversing, and control-containing paths fail.
    #[test]
    fn rejects_invalid_paths_and_formats() {
        for path in [
            "",
            "song.flac",
            "/",
            "/music/",
            "/music/.",
            "/music/../song.flac",
            "/music/a\0b.flac",
        ] {
            assert!(
                NewAudioFile::new(path, "flac", AudioProperties::default())
                    .is_err(),
                "{path:?}"
            );
        }
        let long_path = format!("/{}", "я".repeat(2048));
        assert!(
            NewAudioFile::new(&long_path, "flac", AudioProperties::default())
                .is_err()
        );
        for format in [
            "".into(),
            "audio/flac".into(),
            "f lac".into(),
            "я".into(),
            "a".repeat(33),
        ] {
            assert!(
                NewAudioFile::new(
                    "/music/a.flac",
                    &format,
                    AudioProperties::default()
                )
                .is_err()
            );
        }
    }

    /// Every declared numeric property must be positive when supplied.
    #[test]
    fn validates_all_optional_audio_properties() {
        let valid = AudioProperties {
            size_bytes: Some(1),
            duration_ms: Some(1),
            sample_rate_hz: Some(44100),
            channels: Some(2),
            bit_depth: Some(16),
            bitrate_bps: Some(320000),
        };
        assert!(valid.validate().is_ok());
        for properties in [
            AudioProperties {
                size_bytes: Some(0),
                ..Default::default()
            },
            AudioProperties {
                duration_ms: Some(-1),
                ..Default::default()
            },
            AudioProperties {
                sample_rate_hz: Some(0),
                ..Default::default()
            },
            AudioProperties {
                channels: Some(0),
                ..Default::default()
            },
            AudioProperties {
                bit_depth: Some(0),
                ..Default::default()
            },
            AudioProperties {
                bitrate_bps: Some(0),
                ..Default::default()
            },
        ] {
            assert!(properties.validate().is_err());
        }
    }
}
