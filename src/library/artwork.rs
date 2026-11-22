//! Bounded JPEG/PNG validation; artwork never changes source audio tags.

use anyhow::{Result, ensure};
use image::{ImageFormat, ImageReader, Limits};
use sha2::{Digest, Sha256};
use std::io::Cursor;

/// Maximum encoded artwork size, shared by uploads and external downloads.
pub(crate) const MAX_BYTES: usize = 5 * 1024 * 1024;

/// Validated bytes and cache identity, safe to store and serve as an image.
pub(crate) struct Artwork {
    /// Original encoded JPEG/PNG, never an external URL.
    pub(crate) data: Vec<u8>,
    /// Content-derived type.
    pub(crate) mime: &'static str,
    /// SHA-256 of the encoded bytes.
    pub(crate) digest: String,
}

/// Decode within memory/dimension limits to reject corrupt images and bombs.
/// Run on a blocking worker; no SVG or active content formats are accepted.
pub(crate) fn validate(data: Vec<u8>) -> Result<Artwork> {
    ensure!(
        !data.is_empty() && data.len() <= MAX_BYTES,
        "Artwork must be 1 byte to 5 MiB"
    );
    let format = image::guess_format(&data)?;
    let mime = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        _ => anyhow::bail!("Only PNG and JPEG artwork are supported"),
    };
    let mut reader = ImageReader::with_format(Cursor::new(&data), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    ensure!(image.width() > 0 && image.height() > 0, "Empty image");
    let digest = Sha256::digest(&data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(Artwork { data, mime, digest })
}

/// Extract front-cover artwork from a securely opened regular file.
/// Fall back to the first embedded image if no front-cover type is present.
/// Explicit jobs keep the normal scanner's artwork-free fast path unchanged.
pub(crate) fn embedded(file: std::fs::File) -> Result<Artwork> {
    use lofty::{file::TaggedFileExt, probe::Probe};
    let tagged = Probe::new(std::io::BufReader::new(file))
        .guess_file_type()?
        .read()?;
    let pictures: Vec<_> = tagged
        .tags()
        .iter()
        .flat_map(|tag| tag.pictures())
        .collect();
    let picture = pictures
        .iter()
        .copied()
        .find(|p| p.pic_type() == lofty::picture::PictureType::CoverFront)
        .or_else(|| pictures.first().copied())
        .ok_or_else(|| anyhow::anyhow!("No embedded picture"))?;
    validate(picture.data().to_vec())
}

#[cfg(test)]
impl Artwork {
    /// Write an embedded PNG fixture, then return its complete source bytes.
    pub(crate) fn tagged_wave(path: &std::path::Path) -> Vec<u8> {
        use lofty::{
            config::WriteOptions,
            picture::Picture,
            tag::{Tag, TagExt, TagType},
        };
        std::fs::write(path, crate::library::metadata::tests::wave()).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        let mut input = std::io::Cursor::new(tests::png());
        tag.push_picture(Picture::from_reader(&mut input).unwrap());
        tag.save_to_path(path, WriteOptions::default()).unwrap();
        std::fs::read(path).unwrap()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Encoded image validation and independent embedded-art fixtures.
    use super::*;

    /// Construct a tiny valid PNG without relying on external files.
    pub(crate) fn png() -> Vec<u8> {
        let mut output = Cursor::new(Vec::new());
        image::RgbImage::new(2, 2)
            .write_to(&mut output, ImageFormat::Png)
            .unwrap();
        output.into_inner()
    }

    /// Reject active formats, truncated data, oversized dimensions and bytes.
    #[test]
    fn validates_format_dimensions_and_body_size() {
        let valid = png();
        let art = validate(valid.clone()).unwrap();
        assert_eq!(art.mime, "image/png");
        assert_eq!(art.data, valid);
        assert_eq!(art.digest.len(), 64);
        for invalid in [
            vec![],
            b"<svg/>".to_vec(),
            valid[..valid.len() / 2].to_vec(),
            vec![0; MAX_BYTES + 1],
        ] {
            assert!(validate(invalid).is_err());
        }
        let mut large = Cursor::new(Vec::new());
        image::RgbImage::new(4097, 1)
            .write_to(&mut large, ImageFormat::Png)
            .unwrap();
        assert!(validate(large.into_inner()).is_err());
        let mut jpeg = Cursor::new(Vec::new());
        image::RgbImage::new(2, 2)
            .write_to(&mut jpeg, ImageFormat::Jpeg)
            .unwrap();
        assert_eq!(validate(jpeg.into_inner()).unwrap().mime, "image/jpeg");
    }
}
