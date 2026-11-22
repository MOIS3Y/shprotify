//! Read-only media delivery and client-reported personal playback history.
//!
//! Resolve directories relative to open descriptors and reject symlinks at
//! every component. No canonicalize-then-open check or pathname reopening is
//! used for the media handle. Run these filesystem operations off-thread.

pub(crate) mod history;

use std::{
    fs::File,
    path::{Component, Path, PathBuf},
};

use rustix::{
    fd::OwnedFd,
    fs::{Mode, OFlags, open as open_path, openat},
    io::Errno,
};

/// Missing, disallowed, or unexpected filesystem access failure.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenError {
    /// Disallowed path, unavailable source, symlink, or nonregular file.
    #[error("Audio file is unavailable")]
    Unavailable,
    /// Unexpected filesystem failure; reserved for server diagnostics.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// One regular file handle and its measured size at opening time.
pub(crate) struct OpenedAudio {
    /// Read-only handle; replacement of its pathname cannot redirect reads.
    pub(crate) file: File,
    /// Current physical length, independent of potentially stale catalog data.
    pub(crate) size: u64,
}

/// Open only normal descendants of an allowed root, rejecting special files.
/// Nonblocking open prevents FIFO paths from occupying a worker indefinitely.
pub(crate) fn open(
    roots: &[PathBuf],
    path: &Path,
) -> Result<OpenedAudio, OpenError> {
    let (root, relative) = roots
        .iter()
        .find_map(|root| {
            path.strip_prefix(root)
                .ok()
                .map(|relative| (root, relative))
        })
        .ok_or(OpenError::Unavailable)?;
    let mut components = relative.components();
    let Some(Component::Normal(name)) = components.next_back() else {
        return Err(OpenError::Unavailable);
    };
    let mut directory = open_path("/", directory_flags(), Mode::empty())
        .map_err(filesystem_error)?;
    let absolute =
        root.strip_prefix("/").map_err(|_| OpenError::Unavailable)?;
    directory = walk(directory, absolute.components())?;
    directory = walk(directory, components)?;
    let descriptor = openat(
        &directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(filesystem_error)?;
    let file = File::from(descriptor);
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(OpenError::Unavailable);
    }
    Ok(OpenedAudio {
        file,
        size: metadata.len(),
    })
}

/// Walk normal directory components using descriptor-relative, nofollow opens.
fn walk<'a>(
    mut directory: OwnedFd,
    components: impl Iterator<Item = Component<'a>>,
) -> Result<OwnedFd, OpenError> {
    for component in components {
        let Component::Normal(name) = component else {
            return Err(OpenError::Unavailable);
        };
        directory = openat(&directory, name, directory_flags(), Mode::empty())
            .map_err(filesystem_error)?;
    }
    Ok(directory)
}

/// Directory handles cannot follow symlinks or be inherited by child programs.
fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Treat inaccessible and disallowed source paths as unavailable resources.
fn filesystem_error(error: Errno) -> OpenError {
    match error {
        Errno::NOENT
        | Errno::NOTDIR
        | Errno::LOOP
        | Errno::ACCESS
        | Errno::PERM
        | Errno::NXIO => OpenError::Unavailable,
        other => OpenError::Io(other.into()),
    }
}

/// Known container media types; unknown catalog labels remain opaque binary.
pub(crate) fn content_type(format: &str) -> &'static str {
    match format {
        "mp3" => "audio/mpeg",
        "mp4" => "audio/mp4",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "opus" | "vorbis" | "speex" => "audio/ogg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    //! Filesystem boundaries and stable read handles.

    use super::*;
    use std::{fs, io::Read, os::unix::fs::symlink};

    /// The handle supplies current size and retains its original contents.
    #[test]
    fn reads_a_regular_file_through_a_stable_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("music");
        fs::create_dir_all(root.join("album")).unwrap();
        let path = root.join("album/source.flac");
        fs::write(&path, b"original bytes").unwrap();
        let mut opened = open(std::slice::from_ref(&root), &path).unwrap();
        assert_eq!(opened.size, 14);
        fs::rename(&path, root.join("old.flac")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        let mut bytes = Vec::new();
        opened.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original bytes");
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
    }

    /// Siblings, traversal, links and special files never become media bodies.
    #[test]
    fn rejects_outside_paths_symlinks_and_nonregular_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("music");
        fs::create_dir(&root).unwrap();
        let outside = directory.path().join("music-other");
        fs::create_dir(&outside).unwrap();
        let secret = outside.join("secret");
        fs::write(&secret, b"private").unwrap();
        symlink(&secret, root.join("file-link")).unwrap();
        symlink(&outside, root.join("directory-link")).unwrap();
        let fifo = root.join("fifo");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, Mode::RUSR | Mode::WUSR)
            .unwrap();
        for path in [
            secret,
            root.clone(),
            root.join("missing"),
            fifo,
            root.join("file-link"),
            root.join("directory-link/secret"),
            root.join("../music-other/secret"),
        ] {
            assert!(matches!(
                open(std::slice::from_ref(&root), &path),
                Err(OpenError::Unavailable)
            ));
        }
        let alias = directory.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(matches!(
            open(std::slice::from_ref(&alias), &alias.join("x")),
            Err(OpenError::Unavailable)
        ));
        assert!(matches!(
            open(&[], &root.join("x")),
            Err(OpenError::Unavailable)
        ));
    }
}
