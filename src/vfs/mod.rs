//! The filesystem abstraction both panes and the sync engine talk to.
//!
//! The trait is **blocking**, deliberately. The `remotefs-*` crates that back
//! SFTP, FTP, S3, SMB and WebDAV are themselves blocking, so an async trait
//! would mean wrapping sync calls in `spawn_blocking` and getting async's
//! complexity without its benefit.
//!
//! Responsiveness comes from the transfer queue stepping a chunk at a time on
//! the main loop, not from a worker thread -- the panes own these values, and
//! `Vfs` is `Send` but not `Sync`, so handing one to a thread would mean the
//! browser losing its backend for the duration of every sync. See
//! `sync::transfer`.
//!
//! Every backend is allowed to be missing capabilities -- S3 has no mtime of
//! its own, FTP has no mode bits -- so [`Capabilities`] is part of the trait
//! rather than something callers assume. Code that needs an mtime must cope
//! with `None`, and the confirmation window says which comparison it actually
//! used.

pub mod ftp;
pub mod hostkey;
pub mod local;
pub mod remote;
pub mod ssh;

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};

pub use local::LocalFs;
pub use remote::RemoteAdapter;

/// What an entry in a listing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// A symlink, with [`Entry::symlink_to_dir`] recording whether it resolves
    /// to a directory -- which decides whether entering it is meaningful.
    Symlink,
}

/// One entry in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// File name only, with no directory part.
    pub name: String,
    pub kind: EntryKind,
    /// Apparent size in bytes. Zero for directories unless recursive sizes
    /// were computed.
    pub size: u64,
    /// `None` where the backend has no usable timestamp.
    pub mtime: Option<DateTime<Utc>>,
    /// Unix mode bits, where the backend exposes them.
    pub mode: Option<u32>,
    /// Where a symlink points, unresolved.
    pub symlink_target: Option<PathBuf>,
    /// For a symlink, whether the target is a directory.
    pub symlink_to_dir: bool,
}

impl Entry {
    /// Whether entering this entry navigates into it.
    pub const fn is_dir_like(&self) -> bool {
        matches!(self.kind, EntryKind::Dir) || self.symlink_to_dir
    }

    /// Whether any execute bit is set. Used only for colouring, so a backend
    /// without mode bits simply reports nothing.
    pub fn is_executable(&self) -> bool {
        matches!(self.kind, EntryKind::File) && self.mode.is_some_and(|mode| mode & 0o111 != 0)
    }

    /// Whether this is a dotfile.
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// What to stamp on a file as it is written.
///
/// Gathered from the source file, so a copy carries the original's timestamp
/// and permissions rather than the moment it happened to be transferred. That
/// matters for more than tidiness: a destination stamped with "now" looks
/// newer than its source, and the next sync in the other direction would copy
/// it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WriteMetadata {
    /// Expected size. Required up front by protocols that send a length
    /// before the data.
    pub size: u64,
    /// Modification time to preserve, where the backend can set one.
    pub mtime: Option<DateTime<Utc>>,
    /// Unix mode bits to preserve, where the backend has them.
    pub mode: Option<u32>,
}

impl WriteMetadata {
    /// The metadata a copy of `entry` should carry.
    pub fn from_entry(entry: &Entry) -> Self {
        Self {
            size: entry.size,
            mtime: entry.mtime,
            mode: entry.mode,
        }
    }
}

/// What a backend can and cannot do.
///
/// Carried alongside listings so the planner can decide how to compare files
/// and the UI can explain why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// The backend reports modification times.
    pub mtime: bool,
    /// Modification times can be written back, for `preserve_mtime`.
    pub set_mtime: bool,
    /// Unix mode bits are available.
    pub mode: bool,
    pub symlinks: bool,
    /// Ranged writes, which is what resuming a partial transfer needs.
    pub ranged_writes: bool,
    /// Whether [`Vfs::rename`] replaces an existing target.
    ///
    /// POSIX `rename(2)` does. The SFTP protocol's rename does **not**: the
    /// OpenSSH server fails with a permission error if the target exists,
    /// unless the client negotiates the `posix-rename@openssh.com` extension.
    ///
    /// This matters because the executor writes to a partial name and renames
    /// it into place, so that an interrupted transfer never leaves a truncated
    /// file. Where the rename cannot overwrite, that has to become a
    /// three-step swap instead -- see `executor::transfer`.
    pub atomic_overwrite_rename: bool,
    /// Coarsest timestamp this backend can represent, in seconds.
    ///
    /// Not cosmetic: comparing a 1-second-granularity SFTP server against a
    /// nanosecond local filesystem with a zero tolerance makes every file
    /// look changed. The planner uses this as the floor for its tolerance
    /// window.
    pub mtime_granularity_secs: u64,
}

/// A read-only view of a filesystem.
///
/// Methods were added in the phase that first performed them -- listings in
/// phase 1, writes and deletes in phase 3, streaming handles in phase 4 -- so
/// that nothing could ever call one the backends did not yet implement. What
/// is here is what works against every implemented backend.
pub trait Vfs: Send {
    /// Short name for the status bar: `local`, `sftp`, `s3`.
    fn scheme(&self) -> &str;

    /// How this location is displayed: `/home/greg/proj`, or
    /// `sftp://host/srv/www`.
    fn display_path(&self, path: &Path) -> String;

    fn capabilities(&self) -> Capabilities;

    /// List `path`, excluding `.` and `..`.
    ///
    /// Ordering is not guaranteed; sorting is the caller's business.
    fn list(&mut self, path: &Path) -> Result<Vec<Entry>, VfsError>;

    /// Whether `path` exists and is a directory.
    ///
    /// Separate from `list` because synchronized browsing asks this about a
    /// path it may not want to list -- the counterpart directory that may not
    /// exist on this side.
    fn is_dir(&mut self, path: &Path) -> bool;

    /// Create a directory, and any missing parents.
    ///
    /// The one write phase 1 needs: `sync_browse.on_missing = "create"` is a
    /// documented option, and a backend that cannot honour it would make the
    /// setting a lie.
    fn create_dir(&mut self, path: &Path) -> Result<(), VfsError>;

    /// Metadata for a single entry.
    ///
    /// Separate from [`Vfs::list`] because the planner needs to re-check one
    /// file -- after a checksum disagreement, or to confirm a destination has
    /// not changed since the plan was built.
    fn stat(&mut self, path: &Path) -> Result<Entry, VfsError>;

    /// Read a whole file into `dest`.
    ///
    /// Returns the number of bytes copied, which the executor compares
    /// against the size it planned for: a short read means the file changed
    /// under it, and copying a truncated file over a good one is the worst
    /// outcome available.
    fn read_into(&mut self, path: &Path, dest: &mut (dyn Write + Send)) -> Result<u64, VfsError>;

    /// Open `path` for reading, starting at `offset`.
    ///
    /// Streaming rather than whole-file, because the alternative is reading a
    /// 4 GB file into memory to copy it. `offset` is for resuming an
    /// interrupted transfer; a backend without
    /// [`Capabilities::ranged_writes`] is only ever asked for 0.
    fn open_read(&mut self, path: &Path, offset: u64) -> Result<Box<dyn ReadHandle>, VfsError>;

    /// Open `path` for writing, truncating unless `append` is set.
    ///
    /// `append` resumes a partial transfer. The handle must be finished, not
    /// just dropped: some protocols only commit on close, and a dropped
    /// stream can leave a file the server considers incomplete.
    fn open_write(
        &mut self,
        path: &Path,
        metadata: &WriteMetadata,
        append: bool,
    ) -> Result<Box<dyn WriteHandle>, VfsError>;

    /// Write `src` to `path`, replacing whatever was there.
    ///
    /// `metadata` carries the size hint, mode and mtime to apply. The size
    /// hint is not optional for some protocols -- SCP has to send a length up
    /// front -- which is why it is a parameter rather than something the
    /// backend could discover.
    fn write_from(
        &mut self,
        path: &Path,
        metadata: &WriteMetadata,
        src: &mut (dyn Read + Send),
    ) -> Result<u64, VfsError>;

    /// Set a file's modification time, and mode where supported.
    ///
    /// Called after a write only when the backend could not apply them during
    /// it. A backend without [`Capabilities::set_mtime`] reports
    /// [`VfsError::Unsupported`] rather than silently doing nothing, so the
    /// plan can say the timestamp will not be preserved.
    fn set_times(&mut self, path: &Path, metadata: &WriteMetadata) -> Result<(), VfsError>;

    /// Delete a file.
    fn remove_file(&mut self, path: &Path) -> Result<(), VfsError>;

    /// Delete a directory and everything in it.
    fn remove_dir_all(&mut self, path: &Path) -> Result<(), VfsError>;

    /// Rename within the same filesystem.
    ///
    /// The executor writes to a partial file and renames it into place, so
    /// that an interrupted transfer never leaves a truncated file where a good
    /// one used to be. That only works if the rename is atomic, which it is
    /// for both a local filesystem and SFTP within one server.
    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), VfsError>;

    /// The directory to start in, and where [`crate::keys::Action::GoHome`]
    /// goes.
    fn home(&mut self) -> Option<PathBuf>;

    /// The topmost directory, for [`crate::keys::Action::GoRoot`].
    fn root(&self) -> PathBuf;
}

/// An open read stream.
///
/// `finish` exists because a protocol stream is not just a file descriptor:
/// SFTP has to drain and close its channel, and an error there means the read
/// was incomplete even though every `read` call succeeded.
pub trait ReadHandle: Read + Send {
    fn finish(self: Box<Self>) -> Result<(), VfsError>;
}

/// An open write stream.
///
/// Must be finished rather than dropped. Some protocols commit only on close,
/// so a dropped handle can leave a file the server treats as truncated -- and
/// a truncated file at the destination is the outcome the whole
/// partial-then-rename dance exists to prevent.
pub trait WriteHandle: Write + Send {
    fn finish(self: Box<Self>) -> Result<(), VfsError>;
}

/// Why a filesystem operation failed.
///
/// Variants are the ones the UI distinguishes: a missing directory and a
/// forbidden one say different things in a pane, and in synchronized browsing
/// "not found" is routine while "permission denied" is worth a message.
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    #[error("{0} does not exist")]
    NotFound(String),
    #[error("{0} is not a directory")]
    NotADirectory(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("not connected")]
    NotConnected,
    /// Authentication was refused. Its own variant because it is the one
    /// failure the user can act on immediately, by retyping a password.
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    /// The host key could not be verified. Never silently downgraded to a
    /// connection error -- see [`crate::vfs::hostkey`].
    #[error("host key problem: {0}")]
    HostKey(String),
    /// A protocol lazysync does not speak yet, refused by name.
    #[error("{0}")]
    Unsupported(String),
    #[error("{context}: {message}")]
    Protocol { context: String, message: String },
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl VfsError {
    /// An absent directory, which synchronized browsing treats as ordinary
    /// rather than as an error worth reporting.
    pub const fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound(_) | Self::NotADirectory(_))
    }
}
