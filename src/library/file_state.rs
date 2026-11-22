//! Cheap Unix filesystem observations for incremental audio probing.
//!
//! Size, device/inode identity, and nanosecond mtime/ctime must match exactly.
//! These observations are not content hashes: filesystems with coarse/stale
//! timestamps can miss changes. Forced scans bypass this optimization.

use std::{
    fs::{self, File, Metadata},
    io,
    os::unix::fs::MetadataExt,
    path::Path,
};

use serde::Serialize;

/// Versioned observation of a regular file, excluding access time.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct FileState {
    /// Encoding version; changing the observation policy invalidates old rows.
    version: u8,
    /// File size, independent of whether the audio parser can read it.
    size_bytes: u64,
    /// Filesystem device identifier.
    device: u64,
    /// File identity within the device; detects replacement at the same path.
    inode: u64,
    /// Modification time in Unix seconds.
    modified_seconds: i64,
    /// Subsecond modification time in nanoseconds.
    modified_nanoseconds: i64,
    /// Metadata change time, also detecting writes with restored mtime.
    changed_seconds: i64,
    /// Subsecond metadata change time in nanoseconds.
    changed_nanoseconds: i64,
}

impl FileState {
    /// Read the path itself and reject symlinks/directories/special files.
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    /// Capture metadata from an already open file without consuming bytes.
    pub(crate) fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected a regular audio file",
            ));
        }
        Ok(Self {
            version: 1,
            size_bytes: metadata.len(),
            device: metadata.dev(),
            inode: metadata.ino(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    /// Reject observations changed during probing or replaced at their path.
    ///
    /// The open handle and current path must both still match. Later changes
    /// are detected on the next scan; this is not a filesystem snapshot lock.
    pub(crate) fn verify(&self, file: &File, path: &Path) -> io::Result<()> {
        if *self != Self::from_metadata(&file.metadata()?)?
            || *self != Self::read(path)?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "File changed during audio probing; retry the scan",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Stable reads, replacements, restored mtimes, and active-write guards.

    use std::fs::FileTimes;

    use super::*;

    /// Reading content never invalidates a snapshot through access-time noise.
    #[test]
    fn reading_does_not_change_file_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio");
        fs::write(&path, b"audio").unwrap();
        let before = FileState::read(&path).unwrap();
        fs::read(&path).unwrap();
        assert_eq!(FileState::read(&path).unwrap(), before);
    }

    /// Equal size and restored mtime do not hide a same-inode content write.
    #[test]
    fn detects_same_size_write_with_restored_modification_time() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio");
        fs::write(&path, b"before").unwrap();
        let original = File::open(&path).unwrap().metadata().unwrap();
        let before = FileState::read(&path).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&path, b"after!").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(original.modified().unwrap()),
            )
            .unwrap();
        let after = FileState::read(&path).unwrap();
        assert_eq!(after.inode, before.inode);
        assert_eq!(after.size_bytes, before.size_bytes);
        assert_eq!(after.modified_seconds, before.modified_seconds);
        assert_eq!(after.modified_nanoseconds, before.modified_nanoseconds);
        assert_ne!(after, before);
    }

    /// Atomic replacement invalidates identity and an in-flight open handle.
    #[test]
    fn rejects_path_replacement_during_probing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio");
        let replacement = directory.path().join("replacement");
        fs::write(&path, b"audio").unwrap();
        let file = File::open(&path).unwrap();
        let before =
            FileState::from_metadata(&file.metadata().unwrap()).unwrap();
        fs::write(&replacement, b"other").unwrap();
        fs::rename(replacement, &path).unwrap();
        assert_ne!(FileState::read(&path).unwrap(), before);
        assert!(before.verify(&file, &path).is_err());
    }

    /// Unsupported kinds and missing paths cannot produce cacheable snapshots.
    #[test]
    fn rejects_non_regular_paths() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("audio");
        let alias = directory.path().join("alias");
        fs::write(&file, b"audio").unwrap();
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        assert!(FileState::read(directory.path()).is_err());
        assert!(FileState::read(&alias).is_err());
        assert!(FileState::read(&directory.path().join("missing")).is_err());
    }

    /// Changes to the open file itself invalidate observations before commit.
    #[test]
    fn rejects_content_changed_during_probing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio");
        fs::write(&path, b"before").unwrap();
        let file = File::open(&path).unwrap();
        let before =
            FileState::from_metadata(&file.metadata().unwrap()).unwrap();
        fs::write(&path, b"different audio").unwrap();
        assert!(before.verify(&file, &path).is_err());
    }
}
