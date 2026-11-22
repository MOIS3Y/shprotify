//! Private managed storage; external collection roots are never written.
//!
//! Require an exclusively owned root. Descriptor-relative creation rejects
//! symlinks, and only server-generated names appear below that root. A file
//! is published with a no-replace hard link on the same filesystem, after
//! the import intent is durable in SQLite. The original staging link is
//! removed only after the database commit; recovery can repeat publication.

use anyhow::{Context, Result, ensure};
use rustix::{
    fd::OwnedFd,
    fs::{Mode, OFlags, mkdirat, open, openat},
    io::Errno,
};
use std::{
    fs::{self, File},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

/// Server-exclusive managed filesystem boundary.
#[derive(Debug, Clone)]
pub(crate) struct Storage {
    /// Absolute root, separate from all external collection roots.
    pub(crate) root: PathBuf,
    /// Read-only roots permitted for exact content reuse.
    pub(crate) external: Vec<PathBuf>,
}

/// Create/open normal components without following a symlink at any level.
fn create_directory(path: &Path) -> Result<OwnedFd> {
    let flags = OFlags::RDONLY
        | OFlags::DIRECTORY
        | OFlags::NOFOLLOW
        | OFlags::CLOEXEC;
    let mut fd = open("/", flags, Mode::empty())?;
    for part in path.strip_prefix("/")?.components() {
        let Component::Normal(name) = part else {
            anyhow::bail!("Managed paths must contain normal components");
        };
        match mkdirat(&fd, name, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(error.into()),
        }
        fd = openat(&fd, name, flags, Mode::empty())?;
    }
    Ok(fd)
}

impl Storage {
    /// Prepare a private managed root, rejecting overlap before any creation.
    pub(crate) fn prepare(path: &Path, external: &[PathBuf]) -> Result<Self> {
        let root = std::path::absolute(path)?;
        let root: PathBuf = root
            .components()
            .filter(|p| *p != Component::CurDir)
            .collect();
        ensure!(
            root.to_str().is_some_and(
                |s| s.len() < 3800 && !s.chars().any(char::is_control)
            ) && !root.components().any(|p| p == Component::ParentDir),
            "Invalid managed storage path"
        );
        for other in external {
            ensure!(
                !root.starts_with(other) && !other.starts_with(&root),
                "Managed storage must not overlap external library roots"
            );
        }
        let directory = File::from(create_directory(&root)?);
        let metadata = directory.metadata()?;
        ensure!(
            metadata.mode() & 0o077 == 0,
            "Managed storage must be private (mode 0700)"
        );
        for name in [".staging", "objects"] {
            let fd = File::from(create_directory(&root.join(name))?);
            ensure!(
                fd.metadata()?.mode() & 0o077 == 0,
                "Managed subdirectories must be private"
            );
        }
        Ok(Self {
            root,
            external: external.to_vec(),
        })
    }

    /// One private staging directory per durable, server-assigned job ID.
    pub(crate) fn stage(&self, job: i64) -> Result<PathBuf> {
        let path = self.root.join(".staging").join(job.to_string());
        create_directory(&path)?;
        Ok(path)
    }

    /// Deterministic content name; no source titles become filesystem paths.
    pub(crate) fn object(&self, digest: &str, format: &str) -> PathBuf {
        self.root.join("objects").join(format!("{digest}.{format}"))
    }

    /// Verify the journal target is exactly a generated direct object child.
    pub(crate) fn checked_object(
        &self,
        path: &str,
        digest: &str,
    ) -> Result<PathBuf> {
        let path = PathBuf::from(path);
        ensure!(
            digest.len() == 64
                && digest.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid import digest"
        );
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .context("Missing import format")?;
        ensure!(
            extension.bytes().all(|b| b.is_ascii_alphanumeric())
                && path == self.object(digest, extension),
            "Invalid import path"
        );
        Ok(path)
    }

    /// Publish a verified file without replacing an existing object.
    pub(crate) fn publish(&self, stage: &Path, object: &Path) -> Result<()> {
        let file =
            crate::audio::open(std::slice::from_ref(&self.root), stage)?;
        fs::set_permissions(stage, fs::Permissions::from_mode(0o600))?;
        file.file.sync_all()?;
        match fs::hard_link(stage, object) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // The caller re-probes and checks the complete object digest.
                crate::audio::open(std::slice::from_ref(&self.root), object)?;
            }
            Err(e) => return Err(e.into()),
        }
        File::open(self.root.join("objects"))?.sync_all()?;
        Ok(())
    }

    /// Remove only this job's private scratch tree, never imported objects.
    pub(crate) fn cleanup(&self, job: i64) -> Result<()> {
        let path = self.root.join(".staging").join(job.to_string());
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Disk progress and hard encoded-size bound, including partial files.
    pub(crate) fn stage_size(path: &Path) -> Result<u64> {
        let mut size = 0_u64;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let m = fs::symlink_metadata(entry.path())?;
            ensure!(m.is_file(), "Unexpected staging entry");
            size =
                size.checked_add(m.len()).context("Staging size overflow")?;
        }
        Ok(size)
    }

    /// Select exactly one nonempty regular output; reject partial artifacts.
    pub(crate) fn artifact(path: &Path) -> Result<PathBuf> {
        let files: Vec<_> =
            fs::read_dir(path)?.collect::<std::io::Result<_>>()?;
        ensure!(files.len() == 1, "Expected one complete audio artifact");
        let path = files[0].path();
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file()
                && metadata.len() > 0
                && path.file_name().is_some_and(|n| n
                    .to_string_lossy()
                    .starts_with("audio."))
                && path
                    .extension()
                    .is_some_and(|e| e != "part" && e != "ytdl"),
            "Invalid downloaded artifact"
        );
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    //! Filesystem ownership boundaries and non-replacing publication.
    use super::*;
    use std::os::unix::fs::symlink;

    /// No external-root overlap, traversal or symlink can become managed
    /// storage.
    #[test]
    fn rejects_overlap_links_and_shared_permissions_before_import() {
        let dir = tempfile::tempdir().unwrap();
        let external = dir.path().join("external");
        fs::create_dir(&external).unwrap();
        for root in [&external, &external.join("managed"), dir.path()] {
            assert!(
                Storage::prepare(root, std::slice::from_ref(&external))
                    .is_err()
            );
        }
        assert!(!external.join("managed").exists());
        let alias = dir.path().join("alias");
        symlink(&external, &alias).unwrap();
        assert!(Storage::prepare(&alias, &[]).is_err());
        assert!(Storage::prepare(&alias.join("managed"), &[]).is_err());
        assert!(
            Storage::prepare(&dir.path().join("x/../managed"), &[]).is_err()
        );
        fs::set_permissions(&external, fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(Storage::prepare(&external, &[]).is_err());
    }

    /// Publication retains existing bytes; callers must verify their digest.
    #[test]
    fn publishes_without_replacing_and_rejects_partial_or_linked_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let storage =
            Storage::prepare(&dir.path().join("managed"), &[]).unwrap();
        let stage = storage.stage(1).unwrap();
        let audio = stage.join("audio.wav");
        fs::write(&audio, b"first").unwrap();
        let object = storage.object(&"a".repeat(64), "wav");
        storage.publish(&audio, &object).unwrap();
        assert_eq!(fs::read(&object).unwrap(), b"first");
        fs::remove_file(&audio).unwrap();
        fs::write(&audio, b"second").unwrap();
        storage.publish(&audio, &object).unwrap();
        assert_eq!(fs::read(&object).unwrap(), b"first");
        fs::remove_file(&audio).unwrap();
        symlink(&object, &audio).unwrap();
        assert!(Storage::artifact(&stage).is_err());
        assert!(storage.publish(&audio, &object).is_err());
        storage.cleanup(1).unwrap();
        assert_eq!(fs::read(&object).unwrap(), b"first");
        let stage = storage.stage(2).unwrap();
        fs::write(stage.join("audio.wav.part"), b"incomplete").unwrap();
        assert!(Storage::artifact(&stage).is_err());
        assert!(
            storage
                .checked_object("/outside/file.wav", &"a".repeat(64))
                .is_err()
        );
    }
}
