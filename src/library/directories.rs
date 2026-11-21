//! Read-only preparation of external collection directories for future scans.
//!
//! Paths are resolved against the process working directory. Symlink roots
//! are allowed, but aliases and nested roots are rejected after canonical
//! resolution. Validation opens each directory without traversing its tree
//! and never creates, renames, moves, or writes collection files.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, ensure};

/// Resolve configured roots and verify that each directory can be listed.
///
/// Returns absolute canonical UTF-8 paths in configuration order. Empty
/// configuration is valid. Parent/child overlaps are checked by path
/// components so `/music` and `/music-other` remain separate roots.
///
/// This performs blocking filesystem operations; asynchronous startup should
/// call it through `tokio::task::spawn_blocking` before opening the database.
pub(crate) fn prepare(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for path in paths {
        ensure!(!path.as_os_str().is_empty(), "Library directory is empty");
        let root = fs::canonicalize(path).with_context(|| {
            format!("Failed to resolve library directory {}", path.display())
        })?;
        let name = root.to_str().with_context(|| {
            format!("Library directory is not UTF-8: {}", path.display())
        })?;
        ensure!(
            !name.chars().any(char::is_control),
            "Library directory contains control characters: {}",
            path.display()
        );
        fs::read_dir(&root).with_context(|| {
            format!("Cannot read library directory {}", path.display())
        })?;
        for previous in &roots {
            ensure!(
                !root.starts_with(previous) && !previous.starts_with(&root),
                "Library directories overlap: {} and {}",
                previous.display(),
                root.display()
            );
        }
        roots.push(root);
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    //! Root resolution, overlap detection, and read-only collection handling.

    use super::*;

    /// Canonical roots preserve order and leave existing content untouched.
    #[test]
    fn prepares_existing_directories_without_changing_contents() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("music");
        let second = directory.path().join("music-other");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let file = first.join("keep.flac");
        fs::write(&file, b"unchanged").unwrap();
        let roots = prepare(&[second.clone(), first.join(".")]).unwrap();
        assert_eq!(
            roots,
            [
                fs::canonicalize(&second).unwrap(),
                fs::canonicalize(&first).unwrap()
            ]
        );
        assert_eq!(fs::read(&file).unwrap(), b"unchanged");
        assert_eq!(fs::read_dir(&first).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&second).unwrap().count(), 0);
        assert!(prepare(&[]).unwrap().is_empty());
    }

    /// Missing paths, file paths, and empty inputs report the configured path.
    #[test]
    fn rejects_invalid_roots_without_creating_directories() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file.flac");
        fs::write(&file, b"keep").unwrap();
        let missing = directory.path().join("missing/music");
        for path in [&file, &missing] {
            let error = prepare(std::slice::from_ref(path)).unwrap_err();
            assert!(error.to_string().contains(&path.display().to_string()));
        }
        assert!(!missing.parent().unwrap().exists());
        assert_eq!(fs::read(&file).unwrap(), b"keep");
        assert!(prepare(&[PathBuf::new()]).is_err());
    }

    /// Duplicate and nested roots fail independently of configuration order.
    #[test]
    fn rejects_duplicates_and_overlapping_roots() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("music");
        let child = parent.join("album");
        fs::create_dir_all(&child).unwrap();
        for paths in [
            vec![parent.clone(), parent.join(".")],
            vec![parent.clone(), child.clone()],
            vec![child, parent],
        ] {
            let error = prepare(&paths).unwrap_err();
            assert!(error.to_string().contains("overlap"));
        }
    }

    /// Relative paths are interpreted against the process working directory.
    #[test]
    fn resolves_relative_paths_against_working_directory() {
        let roots = prepare(&[PathBuf::from("examples")]).unwrap();
        assert_eq!(roots, [fs::canonicalize("examples").unwrap()]);
    }

    /// Symlink roots resolve to targets and cannot introduce duplicate scans.
    #[cfg(unix)]
    #[test]
    fn resolves_symlink_roots_and_rejects_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("music");
        let alias = directory.path().join("alias");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert_eq!(
            prepare(std::slice::from_ref(&alias)).unwrap(),
            [fs::canonicalize(&target).unwrap()]
        );
        assert!(prepare(&[target.clone(), alias]).is_err());
        let broken = directory.path().join("broken");
        std::os::unix::fs::symlink(directory.path().join("absent"), &broken)
            .unwrap();
        assert!(prepare(&[broken]).is_err());
        assert!(target.is_dir());
    }

    /// Unrepresentable canonical paths fail before they can reach AudioFile.
    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_symlink_targets_and_control_characters() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join(OsString::from_vec(vec![0xff]));
        let alias = directory.path().join("alias");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert!(prepare(&[alias]).unwrap_err().to_string().contains("UTF-8"));
        let controls = directory.path().join("line\nbreak");
        fs::create_dir(&controls).unwrap();
        assert!(prepare(&[controls]).is_err());
    }
}
