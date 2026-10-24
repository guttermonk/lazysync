//! The filesystem abstraction both panes and the sync engine talk to.
//!
//! The trait is **blocking**, deliberately. The `remotefs-*` crates that back
//! SFTP, FTP, S3, SMB and WebDAV are themselves blocking, so an async trait
//! would mean wrapping sync calls in `spawn_blocking` and getting async's
//! complexity without its benefit. Phase 2 keeps responsiveness by running a
//! backend on its own worker thread and passing results back to the UI
//! thread, which is also how termscp does it.
//!
//! Every backend is allowed to be missing capabilities -- S3 has no mtime of
//! its own, FTP has no mode bits -- so [`Capabilities`] is part of the trait
//! rather than something callers assume. Code that needs an mtime must cope
//! with `None`, and the confirmation window says which comparison it actually
//! used.

pub mod hostkey;
pub mod local;
pub mod ssh;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

pub use local::LocalFs;
pub use ssh::SshFs;

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
/// Phase 1 needs listings only. Writes, deletes and transfers join the trait
/// in the phase that performs them, so that nothing can call a method the
/// backends do not yet implement.
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

    /// The directory to start in, and where [`crate::keys::Action::GoHome`]
    /// goes.
    fn home(&mut self) -> Option<PathBuf>;

    /// The topmost directory, for [`crate::keys::Action::GoRoot`].
    fn root(&self) -> PathBuf;
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
