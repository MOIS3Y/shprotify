//! Generated media and a fixture process with no external service access.

use std::{
    fs,
    io::Cursor,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

/// One second of silent PCM with deterministic catalog tags, not real music.
pub fn wave() -> Vec<u8> {
    let mut chunks = Vec::new();
    let mut format = Vec::new();
    format.extend(1_u16.to_le_bytes());
    format.extend(1_u16.to_le_bytes());
    format.extend(8000_u32.to_le_bytes());
    format.extend(16000_u32.to_le_bytes());
    format.extend(2_u16.to_le_bytes());
    format.extend(16_u16.to_le_bytes());
    chunk(&mut chunks, b"fmt ", &format);
    let mut tags = b"INFO".to_vec();
    chunk(&mut tags, b"INAM", b"Offline song\0");
    chunk(&mut tags, b"IART", b"Offline artist\0");
    chunk(&mut tags, b"IPRD", b"Offline album\0");
    chunk(&mut tags, b"ITRK", b"1\0");
    chunk(&mut chunks, b"LIST", &tags);
    chunk(&mut chunks, b"data", &vec![0; 16000]);
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(u32::try_from(chunks.len() + 4).unwrap().to_le_bytes());
    bytes.extend(b"WAVE");
    bytes.extend(chunks);
    bytes
}

/// Distinct valid content prevents reuse of the external fixture by its hash.
pub fn acquired_wave() -> Vec<u8> {
    let mut bytes = wave();
    *bytes.last_mut().unwrap() = 1;
    bytes
}

/// Append RIFF payload and its required even-byte padding.
fn chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    bytes.extend(kind);
    bytes.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
    bytes.extend(data);
    if !data.len().is_multiple_of(2) {
        bytes.push(0);
    }
}

/// Generate a tiny valid PNG for the public artwork upload/read workflow.
pub fn cover() -> Result<Vec<u8>> {
    let pixels = image::RgbImage::from_pixel(2, 2, image::Rgb([30, 60, 90]));
    let mut output = Cursor::new(Vec::new());
    pixels.write_to(&mut output, image::ImageFormat::Png)?;
    Ok(output.into_inner())
}

/// Resolve trusted fixture tools from PATH, including inside Nix sandbox.
fn executable(name: &str) -> Result<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .with_context(|| format!("Missing fixture tool {name}"))
}

/// Quote generated paths as literal POSIX shell arguments.
fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// Simulate source selection and either a provider failure or validated audio.
pub fn provider(root: &Path) -> Result<PathBuf> {
    let script = root.join("fake-downloader");
    fs::write(root.join("download.wav"), acquired_wave())?;
    fs::write(root.join("provider-mode"), "fail")?;
    let contents = format!(
        r#"#!{shell}
for argument do
    if [ "$argument" = '--dump-single-json' ]; then
        printf '%s\n' '{{"entries":[{{"id":"abcdefghijk",
"title":"Local fixture","channel":"Offline artist","duration":1}}]}}'
        exit 0
    fi
done
if [ "$({cat} {mode})" = fail ]; then
    printf '%s\n' 'fixture-secret-must-not-leak' >&2
    exit 1
fi
{cp} {source} audio.wav
"#,
        shell = executable("sh")?.display(),
        cat = quote(&executable("cat")?),
        cp = quote(&executable("cp")?),
        mode = quote(&root.join("provider-mode")),
        source = quote(&root.join("download.wav")),
    );
    fs::write(&script, contents)?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))?;
    Ok(script)
}
