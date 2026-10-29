//! The local filesystem.

use std::{
    fs,
    io::{self, Read, Seek, Write},
    path::{Component, Path, PathBuf},
};

use super::{
    Capabilities, Entry, EntryKind, ReadHandle, Vfs, VfsError, WriteHandle, WriteMetadata,
};

/// The machine lazysync is running on.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFs;

impl LocalFs {
    pub const fn new() -> Self {
        Self
    }

    /// Resolve `..` and `.` without touching the disk.
    ///
    /// `Path::canonicalize` would also resolve symlinks, which is wrong here:
    /// a user who navigated into a symlinked directory expects the path they
    /// typed to stay on screen, and expects `..` to go back where they came
    /// from rather than to the link target's parent.
    pub fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for component in path.components() {
            match component {
                Component::ParentDir => {
                    out.pop();
                }
                Component::CurDir => {}
                other => out.push(other.as_os_str()),
            }
        }
        if out.as_os_str().is_empty() {
            PathBuf::from("/")
        } else {
            out
        }
    }
}

impl Vfs for LocalFs {
    fn scheme(&self) -> &str {
        "local"
    }

    fn display_path(&self, path: &Path) -> String {
        path.display().to_string()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            mtime: true,
            set_mtime: true,
            mode: cfg!(unix),
            symlinks: true,
            ranged_writes: true,
            // POSIX rename(2) replaces the target atomically.
            atomic_overwrite_rename: true,
            // Local filesystems are finer than this, but claiming 1s rather
            // than 0 avoids pretending a comparison against a coarse remote
            // is exact.
            mtime_granularity_secs: 1,
        }
    }

    fn list(&mut self, path: &Path) -> Result<Vec<Entry>, VfsError> {
        let read = fs::read_dir(path).map_err(|e| classify(e, path))?;

        let mut entries = Vec::new();
        for item in read {
            // One unreadable entry should not lose the whole listing; the
            // directory is still worth showing without it.
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    tracing::warn!("skipping an entry in {}: {e}", path.display());
                    continue;
                }
            };

            let name = item.file_name().to_string_lossy().into_owned();

            // symlink_metadata, so a link is reported as a link rather than as
            // whatever it points at. Where it points is resolved separately.
            let meta = match fs::symlink_metadata(item.path()) {
                Ok(meta) => meta,
                Err(e) => {
                    tracing::warn!("skipping {}: {e}", item.path().display());
                    continue;
                }
            };

            let kind = if meta.is_symlink() {
                EntryKind::Symlink
            } else if meta.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            };

            let (symlink_target, symlink_to_dir) = if meta.is_symlink() {
                let target = fs::read_link(item.path()).ok();
                // A broken link resolves to nothing, which is reported rather
                // than treated as a file.
                let to_dir = fs::metadata(item.path()).is_ok_and(|m| m.is_dir());
                (target, to_dir)
            } else {
                (None, false)
            };

            entries.push(Entry {
                name,
                kind,
                size: if matches!(kind, EntryKind::Dir) {
                    0
                } else {
                    meta.len()
                },
                mtime: meta.modified().ok().map(Into::into),
                mode: mode_of(&meta),
                symlink_target,
                symlink_to_dir,
            });
        }

        Ok(entries)
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        path.is_dir()
    }

    fn create_dir(&mut self, path: &Path) -> Result<(), VfsError> {
        fs::create_dir_all(path).map_err(|e| classify(e, path))
    }

    fn stat(&mut self, path: &Path) -> Result<Entry, VfsError> {
        let meta = fs::symlink_metadata(path).map_err(|e| classify(e, path))?;

        let kind = if meta.is_symlink() {
            EntryKind::Symlink
        } else if meta.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };

        let (symlink_target, symlink_to_dir) = if meta.is_symlink() {
            (
                fs::read_link(path).ok(),
                fs::metadata(path).is_ok_and(|m| m.is_dir()),
            )
        } else {
            (None, false)
        };

        Ok(Entry {
            name: path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            kind,
            size: if matches!(kind, EntryKind::Dir) {
                0
            } else {
                meta.len()
            },
            mtime: meta.modified().ok().map(Into::into),
            mode: mode_of(&meta),
            symlink_target,
            symlink_to_dir,
        })
    }

    fn open_read(&mut self, path: &Path, offset: u64) -> Result<Box<dyn ReadHandle>, VfsError> {
        let mut file = fs::File::open(path).map_err(|e| classify(e, path))?;
        if offset > 0 {
            file.seek(io::SeekFrom::Start(offset))
                .map_err(|e| VfsError::Io {
                    context: format!("seeking to {offset} in {}", path.display()),
                    source: e,
                })?;
        }
        Ok(Box::new(LocalRead(file)))
    }

    fn open_write(
        &mut self,
        path: &Path,
        metadata: &WriteMetadata,
        append: bool,
    ) -> Result<Box<dyn WriteHandle>, VfsError> {
        let file = fs::File::options()
            .write(true)
            .create(true)
            .append(append)
            .truncate(!append)
            .open(path)
            .map_err(|e| classify(e, path))?;

        Ok(Box::new(LocalWrite {
            file,
            // Applied on `finish`, not now: writing data updates the mtime, so
            // stamping it first would be undone by the bytes.
            metadata: *metadata,
            path: path.to_path_buf(),
        }))
    }

    fn read_into(&mut self, path: &Path, dest: &mut (dyn Write + Send)) -> Result<u64, VfsError> {
        let mut file = fs::File::open(path).map_err(|e| classify(e, path))?;
        let copied = io::copy(&mut file, dest).map_err(|e| VfsError::Io {
            context: format!("reading {}", path.display()),
            source: e,
        })?;
        dest.flush().map_err(|e| VfsError::Io {
            context: format!("flushing a copy of {}", path.display()),
            source: e,
        })?;
        Ok(copied)
    }

    fn write_from(
        &mut self,
        path: &Path,
        metadata: &WriteMetadata,
        src: &mut (dyn Read + Send),
    ) -> Result<u64, VfsError> {
        // The parent has to exist; the executor creates directories first, but
        // a plan applied against a tree that changed underneath should fail
        // with a path rather than a bare ENOENT.
        let mut file = fs::File::create(path).map_err(|e| classify(e, path))?;
        let copied = io::copy(src, &mut file).map_err(|e| VfsError::Io {
            context: format!("writing {}", path.display()),
            source: e,
        })?;

        // Flush *and* sync before the caller renames this into place.
        // Otherwise a crash between the rename and the writeback can leave the
        // destination name pointing at a file whose contents never landed.
        file.flush().map_err(|e| VfsError::Io {
            context: format!("flushing {}", path.display()),
            source: e,
        })?;
        file.sync_all().map_err(|e| VfsError::Io {
            context: format!("syncing {}", path.display()),
            source: e,
        })?;
        drop(file);

        self.set_times(path, metadata)?;
        Ok(copied)
    }

    fn set_times(&mut self, path: &Path, metadata: &WriteMetadata) -> Result<(), VfsError> {
        if let Some(mtime) = metadata.mtime {
            let file = fs::File::options()
                .write(true)
                .open(path)
                .map_err(|e| classify(e, path))?;
            let times = fs::FileTimes::new().set_modified(mtime.into());
            file.set_times(times).map_err(|e| VfsError::Io {
                context: format!("setting the timestamp on {}", path.display()),
                source: e,
            })?;
        }

        #[cfg(unix)]
        if let Some(mode) = metadata.mode {
            use std::os::unix::fs::PermissionsExt;
            // Only the permission bits: the file type bits in a stat mode are
            // not ours to set, and passing them through would be rejected.
            fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|e| classify(e, path))?;
        }

        Ok(())
    }

    #[cfg(unix)]
    fn set_mode(&mut self, path: &Path, mode: u32) -> Result<(), VfsError> {
        use std::os::unix::fs::PermissionsExt;
        // Permission bits only, as in `set_times`: the file type bits of a
        // stat mode are not ours to set.
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|e| classify(e, path))
    }

    #[cfg(not(unix))]
    fn set_mode(&mut self, _path: &Path, _mode: u32) -> Result<(), VfsError> {
        Err(VfsError::Unsupported(
            "this platform has no Unix permission bits".into(),
        ))
    }

    fn remove_file(&mut self, path: &Path) -> Result<(), VfsError> {
        fs::remove_file(path).map_err(|e| classify(e, path))
    }

    fn remove_dir_all(&mut self, path: &Path) -> Result<(), VfsError> {
        fs::remove_dir_all(path).map_err(|e| classify(e, path))
    }

    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), VfsError> {
        fs::rename(from, to).map_err(|e| VfsError::Io {
            context: format!("renaming {} to {}", from.display(), to.display()),
            source: e,
        })
    }

    fn external_spec(&self, path: &Path) -> Option<crate::sync::external::Spec> {
        Some(crate::sync::external::Spec::Local(path.to_path_buf()))
    }

    fn home(&mut self) -> Option<PathBuf> {
        dirs::home_dir()
    }

    fn root(&self) -> PathBuf {
        PathBuf::from("/")
    }
}

/// A local file open for reading.
struct LocalRead(fs::File);

impl Read for LocalRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl ReadHandle for LocalRead {
    fn finish(self: Box<Self>) -> Result<(), VfsError> {
        // Nothing to commit on a read.
        Ok(())
    }
}

/// A local file open for writing, with the metadata to stamp on close.
struct LocalWrite {
    file: fs::File,
    metadata: WriteMetadata,
    path: PathBuf,
}

impl Write for LocalWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl WriteHandle for LocalWrite {
    fn finish(mut self: Box<Self>) -> Result<(), VfsError> {
        self.file.flush().map_err(|e| VfsError::Io {
            context: format!("flushing {}", self.path.display()),
            source: e,
        })?;

        // Durable before the caller renames it into place. Without this, a
        // crash between the rename and the writeback leaves the destination
        // name pointing at content that never landed.
        self.file.sync_all().map_err(|e| VfsError::Io {
            context: format!("syncing {}", self.path.display()),
            source: e,
        })?;

        // Timestamps last, on the closed file: writing updates the mtime, so
        // stamping earlier would be undone by the data.
        if let Some(mtime) = self.metadata.mtime {
            let times = fs::FileTimes::new().set_modified(mtime.into());
            self.file.set_times(times).map_err(|e| VfsError::Io {
                context: format!("setting the timestamp on {}", self.path.display()),
                source: e,
            })?;
        }

        #[cfg(unix)]
        if let Some(mode) = self.metadata.mode {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|e| classify(e, &self.path))?;
        }

        Ok(())
    }
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.mode())
}

#[cfg(not(unix))]
fn mode_of(_meta: &fs::Metadata) -> Option<u32> {
    None
}

/// Turn an io error into the variant the UI can act on.
fn classify(e: io::Error, path: &Path) -> VfsError {
    let shown = path.display().to_string();
    match e.kind() {
        io::ErrorKind::NotFound => VfsError::NotFound(shown),
        io::ErrorKind::PermissionDenied => VfsError::PermissionDenied(shown),
        io::ErrorKind::NotADirectory => VfsError::NotADirectory(shown),
        _ => VfsError::Io {
            context: format!("reading {shown}"),
            source: e,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn lists_files_and_directories() {
        let dir = tempdir().unwrap();
        File::create(dir.path().join("a.txt")).unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let mut fs_ = LocalFs::new();
        let mut entries = fs_.list(dir.path()).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].kind, EntryKind::File);
        assert_eq!(entries[1].name, "sub");
        assert_eq!(entries[1].kind, EntryKind::Dir);
        assert!(entries[1].is_dir_like());
    }

    #[test]
    fn a_missing_directory_is_not_found_rather_than_a_generic_error() {
        let mut fs_ = LocalFs::new();
        let err = fs_.list(Path::new("/definitely/not/here")).unwrap_err();
        assert!(err.is_not_found(), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_reported_as_a_symlink_with_its_target() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("target"), dir.path().join("link")).unwrap();

        let mut fs_ = LocalFs::new();
        let entries = fs_.list(dir.path()).unwrap();
        let link = entries.iter().find(|e| e.name == "link").unwrap();

        assert_eq!(link.kind, EntryKind::Symlink);
        assert!(
            link.symlink_to_dir,
            "a link to a directory should be enterable"
        );
        assert!(link.symlink_target.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_symlink_is_listed_and_not_enterable() {
        let dir = tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), dir.path().join("link")).unwrap();

        let mut fs_ = LocalFs::new();
        let entries = fs_.list(dir.path()).unwrap();
        let link = entries.iter().find(|e| e.name == "link").unwrap();

        assert_eq!(link.kind, EntryKind::Symlink);
        assert!(!link.symlink_to_dir);
    }

    /// `..` must be resolved lexically, so that navigating out of a symlinked
    /// directory returns where the user came from.
    #[test]
    fn normalize_resolves_dot_dot_without_following_links() {
        assert_eq!(
            LocalFs::normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            LocalFs::normalize(Path::new("/a/./b")),
            PathBuf::from("/a/b")
        );
        assert_eq!(LocalFs::normalize(Path::new("/..")), PathBuf::from("/"));
        assert_eq!(LocalFs::normalize(Path::new("/")), PathBuf::from("/"));
    }
}
