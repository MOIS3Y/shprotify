//! Read-only, content-based audio probing, independent of catalog matching.
//!
//! Tags retain their source values, including absent or ambiguous metadata.
//! Artwork is not loaded. Unknown properties remain null; audio bitrate is
//! converted from Lofty's kbit/s to the catalog's bit/s unit.

use std::{fs::File, io::BufReader, path::Path};

use anyhow::{Context, Result, bail, ensure};
use lofty::{
    config::ParseOptions,
    file::{AudioFile, FileType, TaggedFileExt},
    prelude::Accessor,
    probe::Probe,
    tag::ItemKey,
};
use serde::Serialize;

use super::audio_files::AudioProperties;

/// Source metadata and measured properties of one successfully parsed file.
#[derive(Debug, Serialize)]
pub(crate) struct ScannedFile {
    /// Absolute UTF-8 path within the configured collection.
    pub(crate) path: String,
    /// Detected container/codec label, independent of the filename extension.
    pub(crate) format: String,
    /// Embedded tags; no filename-based fallback or identity matching.
    pub(crate) tags: AudioTags,
    /// Measured file size and audio properties.
    pub(crate) properties: AudioProperties,
}

/// Common source tags; missing tags are represented explicitly as null.
#[derive(Debug, Default, Serialize)]
pub(crate) struct AudioTags {
    /// Recording title, if embedded.
    pub(crate) title: Option<String>,
    /// Artist credit as written in the selected tag, without splitting names.
    pub(crate) artist: Option<String>,
    /// Album title, if embedded.
    pub(crate) album: Option<String>,
    /// Album-level artist credit, if embedded.
    pub(crate) album_artist: Option<String>,
    /// Track number within the disc.
    pub(crate) track_number: Option<u32>,
    /// Disc number within the release.
    pub(crate) disc_number: Option<u32>,
}

/// Probe a regular file using its contents, without modifying it.
///
/// Unknown content is skipped, except when the extension advertises a
/// supported audio format: such files are reported as damaged/unsupported.
/// Invalid catalog paths and malformed audio return errors. Call only on a
/// blocking worker; this does filesystem IO and synchronous audio parsing.
pub(crate) fn read(path: &Path) -> Result<Option<ScannedFile>> {
    let name = path.to_str().context("File path is not UTF-8")?;
    ensure!(
        path.is_absolute()
            && name.len() <= 4096
            && !name.chars().any(char::is_control),
        "File path must be absolute, at most 4096 bytes, without controls"
    );
    let file = File::open(path).context("Cannot open file for reading")?;
    let size = i64::try_from(file.metadata()?.len())
        .context("File size exceeds the catalog limit")?;
    let probe = Probe::new(BufReader::new(file))
        .options(ParseOptions::new().read_cover_art(false))
        .guess_file_type()
        .context("Cannot detect audio format")?;
    if probe.file_type().is_none() {
        if FileType::from_path(path).is_some() {
            bail!("Unrecognized content in a file with an audio extension");
        }
        return Ok(None);
    }
    let audio = probe.read().context("Cannot read audio metadata")?;
    let tags = audio.primary_tag().or_else(|| audio.first_tag());
    let tags = tags.map_or_else(AudioTags::default, |tag| AudioTags {
        title: tag.title().map(|value| value.into_owned()),
        artist: tag.artist().map(|value| value.into_owned()),
        album: tag.album().map(|value| value.into_owned()),
        album_artist: tag.get_string(ItemKey::AlbumArtist).map(str::to_owned),
        track_number: tag.track(),
        disc_number: tag.disk(),
    });
    let properties = audio.properties();
    let duration = i64::try_from(properties.duration().as_millis())
        .context("Audio duration exceeds the catalog limit")?;
    Ok(Some(ScannedFile {
        path: name.to_owned(),
        format: format_label(audio.file_type())?.to_owned(),
        tags,
        properties: AudioProperties {
            size_bytes: Some(size).filter(|value| *value > 0),
            duration_ms: Some(duration).filter(|value| *value > 0),
            sample_rate_hz: properties
                .sample_rate()
                .map(i64::from)
                .filter(|value| *value > 0),
            channels: properties
                .channels()
                .map(i64::from)
                .filter(|value| *value > 0),
            bit_depth: properties
                .bit_depth()
                .map(i64::from)
                .filter(|value| *value > 0),
            bitrate_bps: properties
                .audio_bitrate()
                .map(|value| i64::from(value) * 1000)
                .filter(|value| *value > 0),
        },
    }))
}

/// Map detected formats to stable labels rather than Rust debug names.
fn format_label(format: FileType) -> Result<&'static str> {
    Ok(match format {
        FileType::Aac => "aac",
        FileType::Aiff => "aiff",
        FileType::Ape => "ape",
        FileType::Flac => "flac",
        FileType::Mpeg => "mp3",
        FileType::Mp4 => "mp4",
        FileType::Mpc => "mpc",
        FileType::Opus => "opus",
        FileType::Vorbis => "vorbis",
        FileType::Speex => "speex",
        FileType::Wav => "wav",
        FileType::WavPack => "wavpack",
        _ => bail!("Unsupported detected audio format"),
    })
}

#[cfg(test)]
pub(super) mod tests {
    //! Independently constructed PCM/RIFF fixtures and malformed input.

    use super::*;

    /// Build one second of mono 16-bit PCM with standard RIFF INFO tags.
    pub(crate) fn wave() -> Vec<u8> {
        let mut chunks = Vec::new();
        let mut pcm_format = Vec::new();
        pcm_format.extend(1_u16.to_le_bytes());
        pcm_format.extend(1_u16.to_le_bytes());
        pcm_format.extend(8000_u32.to_le_bytes());
        pcm_format.extend(16000_u32.to_le_bytes());
        pcm_format.extend(2_u16.to_le_bytes());
        pcm_format.extend(16_u16.to_le_bytes());
        chunk(&mut chunks, b"fmt ", &pcm_format);
        let mut info = b"INFO".to_vec();
        chunk(&mut info, b"INAM", b"Source title\0");
        chunk(&mut info, b"IART", b"Source artist\0");
        chunk(&mut info, b"IPRD", b"Source album\0");
        chunk(&mut info, b"ITRK", b"7\0");
        chunk(&mut chunks, b"LIST", &info);
        chunk(&mut chunks, b"data", &vec![0; 16000]);
        let mut bytes = b"RIFF".to_vec();
        bytes.extend(u32::try_from(chunks.len() + 4).unwrap().to_le_bytes());
        bytes.extend(b"WAVE");
        bytes.extend(chunks);
        bytes
    }

    /// Append a RIFF chunk and its mandatory even-byte padding.
    fn chunk(output: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
        output.extend(id);
        output.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
        output.extend(payload);
        if !payload.len().is_multiple_of(2) {
            output.push(0);
        }
    }

    /// Content wins over extension and source tags remain unmodified.
    #[test]
    fn reads_tags_and_properties_from_contents_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("song.flac");
        let bytes = wave();
        std::fs::write(&path, &bytes).unwrap();
        let audio = read(&path).unwrap().unwrap();
        assert_eq!(audio.format, "wav");
        assert_eq!(audio.path, path.to_str().unwrap());
        assert_eq!(audio.tags.title.as_deref(), Some("Source title"));
        assert_eq!(audio.tags.artist.as_deref(), Some("Source artist"));
        assert_eq!(audio.tags.album.as_deref(), Some("Source album"));
        assert_eq!(audio.tags.track_number, Some(7));
        assert!(audio.tags.album_artist.is_none());
        assert!(audio.tags.disc_number.is_none());
        assert_eq!(audio.properties.size_bytes, Some(bytes.len() as i64));
        assert_eq!(audio.properties.duration_ms, Some(1000));
        assert_eq!(audio.properties.sample_rate_hz, Some(8000));
        assert_eq!(audio.properties.channels, Some(1));
        assert_eq!(audio.properties.bit_depth, Some(16));
        assert_eq!(audio.properties.bitrate_bps, Some(128000));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    /// Missing tags stay absent instead of inferring catalog identities.
    #[test]
    fn reads_untagged_audio_without_an_extension() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio");
        let mut bytes = wave();
        let list = bytes.windows(4).position(|part| part == b"LIST").unwrap();
        let data = bytes.windows(4).position(|part| part == b"data").unwrap();
        bytes.drain(list..data);
        let size = u32::try_from(bytes.len() - 8).unwrap();
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let audio = read(&path).unwrap().unwrap();
        assert_eq!(audio.format, "wav");
        assert!(audio.tags.title.is_none());
        assert!(audio.tags.artist.is_none());
        assert_eq!(audio.properties.duration_ms, Some(1000));
    }

    /// Non-audio is skipped, but audio extensions and broken headers fail.
    #[test]
    fn distinguishes_unsupported_and_corrupt_files() {
        let directory = tempfile::tempdir().unwrap();
        for (name, bytes, fails) in [
            ("cover.txt", b"not audio".as_slice(), false),
            ("broken.FLAC", b"not audio".as_slice(), true),
            ("truncated", b"RIFF\xff\xff\xff\xffWAVE".as_slice(), true),
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            let result = read(&path);
            if fails {
                assert!(result.is_err(), "{name}: {result:?}");
            } else {
                assert!(result.unwrap().is_none());
            }
        }
        assert!(read(&directory.path().join("missing.wav")).is_err());
    }

    /// Paths unsupported by the catalog fail explicitly rather than changing.
    #[test]
    fn rejects_control_characters_and_relative_paths() {
        assert!(read(Path::new("relative.wav")).is_err());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("line\nbreak.wav");
        std::fs::write(&path, wave()).unwrap();
        assert!(read(&path).is_err());
    }
}
