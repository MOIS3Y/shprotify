//! Sequential read-only traversal with a streaming report and isolated errors.
//!
//! Roots must come from `directories::prepare`. Nested symlinks and special
//! files are skipped, so ordinary collection links cannot escape the roots
//! or form loops. This is not a filesystem sandbox against concurrent path
//! replacement. No file, directory, or catalog entry is created or changed.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde::Serialize;

use super::metadata::{self, ScannedFile};

/// Aggregate counts for one traversal; an empty collection is successful.
#[derive(Debug, Default, Clone, Copy, Serialize)]
pub(crate) struct Summary {
    /// Successfully probed audio files.
    pub(crate) audio_files: usize,
    /// Unsupported files, nested symlinks, and special files.
    pub(crate) skipped: usize,
    /// Failed entries, directory listings, or audio probes.
    pub(crate) errors: usize,
    /// Previously probed files bypassed by the background scan policy.
    /// Full CLI reports keep their existing schema and always read all files.
    #[serde(skip)]
    pub(crate) unchanged: usize,
}

/// One JSON Lines record; paths in errors use lossy display if not UTF-8.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Event {
    /// One parsed file; boxed to keep the event enum compact.
    Audio {
        /// Original tags and measured audio properties.
        file: Box<ScannedFile>,
    },
    /// A failure that does not stop traversal of other entries.
    Error {
        /// File or directory for which the operation failed.
        path: String,
        /// Diagnostic text intended for the local administrator.
        message: String,
    },
    /// Final counts emitted even when individual entries failed.
    Summary {
        /// Totals for this scan.
        #[serde(flatten)]
        counts: Summary,
    },
}

/// Traverse prepared roots and emit records without collecting audio results.
///
/// Directory iteration order is unspecified. Entries that fail are reported
/// and counted; errors from the report sink stop immediately. The final
/// summary lets callers choose a failing exit status for incomplete scans.
/// Blocking IO and parsing must run outside Tokio's async worker threads.
pub(crate) fn run(
    roots: &[PathBuf],
    emit: impl FnMut(Event) -> Result<()>,
) -> Result<Summary> {
    run_incremental(roots, |_| Ok(false), emit)
}

/// Traverse with a predicate that recognizes committed file observations.
///
/// Predicate failures are fatal (for example a failed database lookup). Probe
/// failures remain isolated per-entry errors. Cache hits increment only the
/// summary; no audio event or tag read occurs for unchanged files.
pub(crate) fn run_incremental(
    roots: &[PathBuf],
    mut unchanged: impl FnMut(&Path) -> Result<bool>,
    mut emit: impl FnMut(Event) -> Result<()>,
) -> Result<Summary> {
    let mut pending = roots.to_vec();
    let mut counts = Summary::default();
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                report_error(
                    &directory,
                    error.into(),
                    &mut counts,
                    &mut emit,
                )?;
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    report_error(
                        &directory,
                        error.into(),
                        &mut counts,
                        &mut emit,
                    )?;
                    continue;
                }
            };
            let path = entry.path();
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    report_error(&path, error.into(), &mut counts, &mut emit)?;
                    continue;
                }
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                if unchanged(&path)? {
                    counts.unchanged += 1;
                    continue;
                }
                match metadata::read(&path) {
                    Ok(Some(file)) => {
                        emit(Event::Audio {
                            file: Box::new(file),
                        })?;
                        counts.audio_files += 1;
                    }
                    Ok(None) => counts.skipped += 1,
                    Err(error) => {
                        report_error(&path, error, &mut counts, &mut emit)?;
                    }
                }
            } else {
                counts.skipped += 1;
            }
        }
    }
    emit(Event::Summary { counts })?;
    Ok(counts)
}

/// Count and emit a filesystem/probe failure, preserving its error chain.
fn report_error(
    path: &std::path::Path,
    error: anyhow::Error,
    counts: &mut Summary,
    emit: &mut impl FnMut(Event) -> Result<()>,
) -> Result<()> {
    counts.errors += 1;
    emit(Event::Error {
        path: path.to_string_lossy().into_owned(),
        message: format!("{error:#}"),
    })
}

#[cfg(test)]
mod tests {
    //! Traversal isolation, error reporting, and collection preservation.

    use super::*;

    /// A cache hit bypasses even a file whose contents would fail probing.
    #[test]
    fn incremental_hits_do_not_probe_audio_or_emit_file_events() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("song.wav"), b"not audio").unwrap();
        let mut events = Vec::new();
        let summary = run_incremental(
            &[directory.path().to_owned()],
            |_| Ok(true),
            |event| {
                events.push(event);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(summary.unchanged, 1);
        assert_eq!(summary.audio_files, 0);
        assert_eq!(summary.errors, 0);
        assert!(matches!(events.as_slice(), [Event::Summary { .. }]));
    }

    /// Failed cache storage cannot silently turn into skipped collection data.
    #[test]
    fn incremental_lookup_errors_stop_traversal() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("song.wav"), b"audio").unwrap();
        let error = run_incremental(
            &[directory.path().to_owned()],
            |_| anyhow::bail!("cache lookup failed"),
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "cache lookup failed");
    }

    /// Nested collections emit files and errors once, then summary counts.
    #[test]
    fn traverses_nested_files_and_continues_after_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let album = directory.path().join("artist/album");
        fs::create_dir_all(&album).unwrap();
        let audio = album.join("song");
        let bytes = metadata::tests::wave();
        fs::write(&audio, &bytes).unwrap();
        fs::write(directory.path().join("broken.mp3"), b"damaged").unwrap();
        fs::write(album.join("cover.txt"), b"keep").unwrap();
        let roots = crate::library::directories::prepare(&[directory
            .path()
            .to_owned()])
        .unwrap();
        let mut events = Vec::new();
        let counts = run(&roots, |event| {
            events.push(serde_json::to_value(event)?);
            Ok(())
        })
        .unwrap();
        assert_eq!(counts.audio_files, 1);
        assert_eq!(counts.skipped, 1);
        assert_eq!(counts.errors, 1);
        assert_eq!(events.len(), 3);
        assert_eq!(events.last().unwrap()["type"], "summary");
        assert_eq!(events.last().unwrap()["errors"], 1);
        assert_eq!(events.iter().filter(|e| e["type"] == "audio").count(), 1);
        assert_eq!(fs::read(audio).unwrap(), bytes);
        assert_eq!(fs::read(album.join("cover.txt")).unwrap(), b"keep");
        assert_eq!(fs::read_dir(album).unwrap().count(), 2);
    }

    /// Empty configuration succeeds and vanished roots become report errors.
    #[test]
    fn reports_empty_and_unavailable_roots() {
        let mut events = Vec::new();
        let summary = run(&[], |event| {
            events.push(event);
            Ok(())
        })
        .unwrap();
        assert_eq!(summary.errors, 0);
        assert!(matches!(events.as_slice(), [Event::Summary { .. }]));
        let directory = tempfile::tempdir().unwrap();
        let summary =
            run(&[directory.path().join("absent")], |_| Ok(())).unwrap();
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.audio_files, 0);
    }

    /// A failed report sink aborts instead of silently losing data.
    #[test]
    fn propagates_report_sink_errors() {
        let error = run(&[], |_| anyhow::bail!("report closed")).unwrap_err();
        assert_eq!(error.to_string(), "report closed");
    }

    /// Links cannot recurse, escape a root, or duplicate audio entries.
    #[cfg(unix)]
    #[test]
    fn skips_symlinks_and_non_utf8_files_without_leaving_roots() {
        use std::{
            ffi::OsString,
            os::unix::{ffi::OsStringExt, fs::symlink},
        };
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let audio = outside.path().join("outside.wav");
        fs::write(&audio, metadata::tests::wave()).unwrap();
        symlink(outside.path(), directory.path().join("escape")).unwrap();
        symlink(directory.path(), directory.path().join("loop")).unwrap();
        symlink(&audio, directory.path().join("alias.wav")).unwrap();
        symlink("absent", directory.path().join("broken")).unwrap();
        let invalid = directory.path().join(OsString::from_vec(vec![0xff]));
        fs::write(invalid, metadata::tests::wave()).unwrap();
        let summary = run(&[directory.path().to_owned()], |_| Ok(())).unwrap();
        assert_eq!(summary.audio_files, 0);
        assert_eq!(summary.skipped, 4);
        assert_eq!(summary.errors, 1);
        assert_eq!(fs::read(audio).unwrap(), metadata::tests::wave());
    }
}
