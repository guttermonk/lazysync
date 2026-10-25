//! One [`Vfs`] implementation for every `remotefs` backend.
//!
//! This exists because `remotefs-ssh`, `-ftp`, `-webdav`, `-aws-s3` and
//! `-smb` all implement the same `RemoteFs` trait. Everything lazysync needs
//! on top of that -- translating entries and errors, streaming reads and
//! writes, resolving a starting directory -- is identical across them, so
//! writing it per protocol would be four copies of the same code drifting
//! apart.
//!
//! What stays protocol-specific is only what genuinely differs:
//!
//! - **Construction.** Each crate takes its own options type, and SSH needs a
//!   host key check before it connects at all.
//! - **Capabilities.** The planner's comparison rules turn on these, and
//!   getting one wrong is how a sync re-copies an unchanged tree. They are
//!   declared per protocol in [`Protocol::capabilities`] rather than guessed
//!   from the `remotefs` flags, because `remotefs` describes streams and
//!   seeking while the planner asks about timestamps and their precision.
//!
//! So each backend module reduces to "build the client, hand it here".

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

use remotefs::{
    fs::{File, FileType, ReadOptions, SetMetadata, UnixPex, WriteOptions},
    RemoteError, RemoteErrorType, RemoteFs,
};

use super::{
    Capabilities, Entry, EntryKind, ReadHandle, Vfs, VfsError, WriteHandle, WriteMetadata,
};
use crate::connection::{Protocol, Target};

/// A connected remote filesystem.
pub struct RemoteAdapter {
    client: Box<dyn RemoteFs + Send>,
    protocol: Protocol,
    target: Target,
    /// Where `GoHome` goes, and the default starting directory.
    home: Option<PathBuf>,
}

impl RemoteAdapter {
    /// Wrap a connected client.
    ///
    /// `home` is resolved by the caller, because how you find it is the one
    /// remaining protocol difference: SSH runs `pwd`, FTP asks for the working
    /// directory, and S3 has no such concept.
    pub fn new(
        client: Box<dyn RemoteFs + Send>,
        protocol: Protocol,
        target: Target,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            client,
            protocol,
            target,
            home,
        }
    }

    pub const fn target(&self) -> &Target {
        &self.target
    }

    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_connected()
    }

    pub fn disconnect(&mut self) {
        if let Err(e) = self.client.disconnect() {
            // Worth a log line and nothing more: the session is being thrown
            // away regardless.
            tracing::debug!("disconnect from {}: {e}", self.target.display());
        }
    }

    /// Ask the server for its idea of the current directory, **by running
    /// `pwd` as a shell command**.
    ///
    /// SSH only. `RemoteFs` has no `pwd` of its own, so this goes out over an
    /// exec channel -- which means it works exactly where a shell does.
    /// `None` where the server declines: an SFTP-only account with no shell
    /// cannot answer, which is common and not an error.
    ///
    /// A protocol with a working directory of its own must not use this. FTP
    /// has `PWD`, and sending it the string `pwd` over a non-existent exec
    /// channel just fails -- see `ftp::open`, which reads it off the live
    /// client before it is boxed up in here.
    pub fn probe_home_via_shell_pwd(&mut self) -> Option<PathBuf> {
        let output = self.client.exec("pwd").ok()?;
        let path = output.stdout.trim();
        if path.starts_with('/') {
            Some(PathBuf::from(path))
        } else {
            tracing::debug!("`pwd` on {} gave {path:?}", self.target.display());
            None
        }
    }

    pub fn set_home(&mut self, home: Option<PathBuf>) {
        self.home = home;
    }

    /// Open the session.
    ///
    /// Separate from [`RemoteAdapter::new`] so a backend module can wrap the
    /// error with its own context -- `remotefs`'s message for a refused FTP
    /// connection is just "connection error", which tells the user nothing
    /// about which host.
    pub fn connect_now(&mut self) -> Result<(), VfsError> {
        self.client
            .connect()
            .map(|_| ())
            .map_err(|e| translate(e, "connecting"))
    }
}

impl Vfs for RemoteAdapter {
    fn scheme(&self) -> &str {
        self.protocol.scheme()
    }

    fn display_path(&self, path: &Path) -> String {
        // Host plus path, so two panes showing `/srv/www` cannot be confused
        // for each other.
        format!("{}:{}", self.target.host, path.display())
    }

    fn capabilities(&self) -> Capabilities {
        self.protocol.capabilities()
    }

    fn list(&mut self, path: &Path) -> Result<Vec<Entry>, VfsError> {
        let files = self
            .client
            .list_dir(path)
            .map_err(|e| translate(e, &format!("listing {}", path.display())))?;

        Ok(files.iter().map(convert).collect())
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        // `stat` rather than `exists`: synchronized browsing needs to know it
        // is a *directory*, because following the other pane into a path that
        // happens to be a file would show an empty listing with no
        // explanation.
        match self.client.stat(path) {
            Ok(file) => matches!(file.metadata.file_type, FileType::Directory),
            Err(_) => false,
        }
    }

    fn create_dir(&mut self, path: &Path) -> Result<(), VfsError> {
        // 0o755, matching `mkdir` with a standard umask. The server applies
        // its own on top.
        let mode = UnixPex::from(0o755);

        // remotefs creates one level only, so walk the missing ancestors.
        let mut missing = Vec::new();
        let mut current = Some(path);
        while let Some(dir) = current {
            if self.is_dir(dir) {
                break;
            }
            missing.push(dir.to_path_buf());
            current = dir.parent();
        }

        for dir in missing.iter().rev() {
            match self.client.create_dir(dir, Some(mode)) {
                Ok(()) => {}
                // Not an error: two panes following each other can both ask
                // for the same path, and a race between them is harmless.
                Err(e) if matches!(e.kind(), RemoteErrorType::AlreadyExists) => {}
                Err(e) => return Err(translate(e, &format!("creating {}", dir.display()))),
            }
        }

        Ok(())
    }

    fn stat(&mut self, path: &Path) -> Result<Entry, VfsError> {
        let file = self
            .client
            .stat(path)
            .map_err(|e| translate(e, &format!("stat {}", path.display())))?;
        Ok(convert(&file))
    }

    fn open_read(&mut self, path: &Path, offset: u64) -> Result<Box<dyn ReadHandle>, VfsError> {
        let mut opts = ReadOptions::default();
        if offset > 0 {
            opts = opts.offset(offset);
        }
        let stream = self
            .client
            .open(path, &opts)
            .map_err(|e| translate(e, &format!("opening {}", path.display())))?;
        Ok(Box::new(RemoteRead {
            stream: Some(stream),
            path: path.to_path_buf(),
        }))
    }

    fn open_write(
        &mut self,
        path: &Path,
        metadata: &WriteMetadata,
        append: bool,
    ) -> Result<Box<dyn WriteHandle>, VfsError> {
        // The size hint is not a nicety: SCP sends a length before the data,
        // so a wrong one corrupts the transfer rather than merely slowing it.
        let mut opts = WriteOptions::default().size_hint(metadata.size);

        // Applied during the write where the protocol allows it: one round
        // trip instead of two, and no window where the file exists with the
        // wrong timestamp.
        if let Some(mtime) = metadata.mtime {
            opts = opts.modified(mtime.into());
        }
        if let Some(mode) = metadata.mode {
            opts = opts.mode(UnixPex::from(mode & 0o7777));
        }

        let stream = if append {
            self.client.append(path, &opts)
        } else {
            self.client.create(path, &opts)
        }
        .map_err(|e| translate(e, &format!("opening {} for writing", path.display())))?;

        Ok(Box::new(RemoteWrite {
            stream: Some(stream),
            path: path.to_path_buf(),
        }))
    }

    fn read_into(&mut self, path: &Path, dest: &mut (dyn Write + Send)) -> Result<u64, VfsError> {
        self.client
            .read_file(path, &ReadOptions::default(), dest)
            .map_err(|e| translate(e, &format!("reading {}", path.display())))
    }

    fn write_from(
        &mut self,
        path: &Path,
        metadata: &WriteMetadata,
        src: &mut (dyn Read + Send),
    ) -> Result<u64, VfsError> {
        let mut opts = WriteOptions::default().size_hint(metadata.size);
        if let Some(mtime) = metadata.mtime {
            opts = opts.modified(mtime.into());
        }
        if let Some(mode) = metadata.mode {
            opts = opts.mode(UnixPex::from(mode & 0o7777));
        }

        self.client
            .write_file(path, &opts, src)
            .map_err(|e| translate(e, &format!("writing {}", path.display())))
    }

    fn set_times(&mut self, path: &Path, metadata: &WriteMetadata) -> Result<(), VfsError> {
        if !self.capabilities().set_mtime {
            return Err(VfsError::Unsupported(format!(
                "{} cannot set timestamps",
                self.protocol
            )));
        }

        let mut set = SetMetadata::default();
        if let Some(mtime) = metadata.mtime {
            set = set.modified(mtime.into());
        }
        if let Some(mode) = metadata.mode {
            set = set.mode(UnixPex::from(mode & 0o7777));
        }

        self.client
            .set_metadata(path, &set)
            .map_err(|e| translate(e, &format!("setting metadata on {}", path.display())))
    }

    fn remove_file(&mut self, path: &Path) -> Result<(), VfsError> {
        self.client
            .remove_file(path)
            .map_err(|e| translate(e, &format!("removing {}", path.display())))
    }

    fn remove_dir_all(&mut self, path: &Path) -> Result<(), VfsError> {
        self.client
            .remove_dir_all(path)
            .map_err(|e| translate(e, &format!("removing {}", path.display())))
    }

    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), VfsError> {
        self.client.rename(from, to).map_err(|e| {
            translate(
                e,
                &format!("renaming {} to {}", from.display(), to.display()),
            )
        })
    }

    fn external_spec(&self, path: &Path) -> Option<crate::sync::external::Spec> {
        use crate::connection::AuthMethod;

        // SSH only. Neither rsync nor rclone can be handed an FTP location in
        // a form that reuses what lazysync already knows about it, and
        // guessing would point a tool somewhere other than the pane on screen.
        if !self.protocol.is_ssh() {
            return None;
        }

        Some(crate::sync::external::Spec::Ssh {
            user: self.target.username.clone(),
            host: self.target.host.clone(),
            port: self.target.port_or_default(),
            identity: self.target.identity_file.clone(),
            // A typed password cannot be handed on: rsync would open its own
            // session and prompt for it again, over a screen the TUI owns.
            password_auth: matches!(self.target.auth, AuthMethod::Password),
            path: path.to_path_buf(),
        })
    }

    fn home(&mut self) -> Option<PathBuf> {
        self.home.clone()
    }

    fn root(&self) -> PathBuf {
        PathBuf::from("/")
    }
}

impl Drop for RemoteAdapter {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// A remote file open for reading.
///
/// The stream is in an `Option` because `finish` consumes it and the trait
/// method takes a boxed `self`; there is nowhere else to move it out of.
struct RemoteRead {
    stream: Option<remotefs::fs::ReadStream>,
    path: PathBuf,
}

impl Read for RemoteRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match &mut self.stream {
            Some(stream) => stream.read(buf),
            None => Ok(0),
        }
    }
}

impl ReadHandle for RemoteRead {
    fn finish(mut self: Box<Self>) -> Result<(), VfsError> {
        match self.stream.take() {
            Some(stream) => stream
                .finish()
                .map_err(|e| translate(e, &format!("closing the read of {}", self.path.display()))),
            None => Ok(()),
        }
    }
}

/// A remote file open for writing.
struct RemoteWrite {
    stream: Option<remotefs::fs::WriteStream>,
    path: PathBuf,
}

impl Write for RemoteWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &mut self.stream {
            Some(stream) => stream.write(buf),
            None => Err(std::io::Error::other("the stream was already finished")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.stream {
            Some(stream) => stream.flush(),
            None => Ok(()),
        }
    }
}

impl WriteHandle for RemoteWrite {
    fn finish(mut self: Box<Self>) -> Result<(), VfsError> {
        let Some(mut stream) = self.stream.take() else {
            return Ok(());
        };

        // Flush before finishing: `finish` closes the channel, and buffered
        // bytes that never reached the server would leave a short file the
        // caller then renames into place.
        stream.flush().map_err(|e| VfsError::Io {
            context: format!("flushing {}", self.path.display()),
            source: e,
        })?;

        stream
            .finish()
            .map_err(|e| translate(e, &format!("closing {}", self.path.display())))
    }
}

impl Drop for RemoteWrite {
    fn drop(&mut self) {
        if self.stream.is_some() {
            // A dropped write stream is a bug: the caller is about to rename a
            // file whose last bytes may never have been sent.
            tracing::warn!(
                "{} was dropped without being finished; it may be incomplete",
                self.path.display()
            );
        }
    }
}

/// Turn a `remotefs` entry into ours.
pub fn convert(file: &File) -> Entry {
    let metadata = &file.metadata;
    let kind = match metadata.file_type {
        FileType::Directory => EntryKind::Dir,
        FileType::Symlink => EntryKind::Symlink,
        // `FileType` is non-exhaustive, so a future variant has to land
        // somewhere. `File` is the safe default: browsable and not enterable,
        // which is how an unrecognised entry should behave.
        FileType::File | _ => EntryKind::File,
    };

    Entry {
        name: file.path.file_name().map_or_else(
            || file.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        kind,
        size: if matches!(kind, EntryKind::Dir) {
            0
        } else {
            metadata.size.unwrap_or(0)
        },
        mtime: metadata.modified.map(Into::into),
        mode: metadata.mode.map(u32::from),
        symlink_target: metadata.symlink.clone(),
        // A listing gives the link target but not what it resolves to, and
        // stat'ing every link would cost a round trip per entry -- slow enough
        // to feel on a directory of a few hundred. Entering a link falls back
        // to `is_dir`.
        symlink_to_dir: false,
    }
}

/// Map a `remotefs` error onto the variants the UI distinguishes.
pub fn translate(error: RemoteError, context: &str) -> VfsError {
    match error.kind() {
        // `StatFailed` joins the not-found cases on purpose: a server
        // answering a stat for a path that is not there is the usual way
        // "missing" arrives, and synchronized browsing has to read it as
        // absent-and-fine rather than as an error worth a status message.
        RemoteErrorType::NoSuchFileOrDirectory
        | RemoteErrorType::CouldNotOpenFile
        | RemoteErrorType::StatFailed => VfsError::NotFound(context.to_owned()),
        RemoteErrorType::PermissionDenied | RemoteErrorType::FileCreateDenied => {
            VfsError::PermissionDenied(context.to_owned())
        }
        // "There is no session" -- nothing further to say, and the only case
        // callers rewrite into a friendlier message.
        RemoteErrorType::NotConnected => VfsError::NotConnected,

        // `ConnectionError` is *not* collapsed into the above, because
        // `remotefs` uses it for anything that went wrong while establishing a
        // session -- including a failed TLS handshake, where the reason lives
        // in the error's source. Collapsing it threw that away and reported a
        // rejected certificate as "the server refused the connection", which
        // sends you to look at your firewall over something else entirely.
        // `tests/ftp.rs` pins this.
        RemoteErrorType::ConnectionError => VfsError::Protocol {
            context: context.to_owned(),
            message: error.to_string(),
        },
        // Authentication is the one a user can act on immediately, so it keeps
        // its own variant rather than becoming a generic protocol error.
        RemoteErrorType::AuthenticationFailed => VfsError::AuthFailed(error.to_string()),
        RemoteErrorType::UnsupportedFeature => VfsError::Unsupported(error.to_string()),
        _ => VfsError::Protocol {
            context: context.to_owned(),
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use remotefs::fs::Metadata;

    use super::*;

    fn file(name: &str, file_type: FileType) -> File {
        File::new(
            PathBuf::from("/srv/www").join(name),
            Metadata::default().file_type(file_type),
        )
    }

    #[test]
    fn a_listing_entry_keeps_only_its_name() {
        let entry = convert(&file("index.html", FileType::File));
        assert_eq!(entry.name, "index.html");
        assert_eq!(entry.kind, EntryKind::File);
    }

    #[test]
    fn a_directory_reports_no_apparent_size() {
        let mut dir = file("src", FileType::Directory);
        dir.metadata.size = Some(4096);

        let entry = convert(&dir);
        assert_eq!(entry.kind, EntryKind::Dir);
        assert_eq!(
            entry.size, 0,
            "a directory's apparent size is meaningless, and 4096 would read as a real size"
        );
        assert!(entry.is_dir_like());
    }

    #[test]
    fn size_mode_and_mtime_come_through() {
        let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut f = file("notes.md", FileType::File);
        f.metadata.size = Some(1234);
        f.metadata.modified = Some(when);
        f.metadata.mode = Some(UnixPex::from(0o644));

        let entry = convert(&f);
        assert_eq!(entry.size, 1234);
        assert_eq!(entry.mode, Some(0o644));
        assert_eq!(entry.mtime.unwrap().timestamp(), 1_700_000_000);
    }

    #[test]
    fn an_executable_is_detected_from_its_mode() {
        let mut f = file("deploy.sh", FileType::File);
        f.metadata.mode = Some(UnixPex::from(0o755));
        assert!(convert(&f).is_executable());

        let mut f = file("notes.md", FileType::File);
        f.metadata.mode = Some(UnixPex::from(0o644));
        assert!(!convert(&f).is_executable());
    }

    /// A server with no timestamps must produce `None`, not epoch zero: the
    /// confirmation window renders the difference, and 1970 would read as a
    /// real date.
    #[test]
    fn a_missing_timestamp_stays_missing() {
        let entry = convert(&file("x", FileType::File));
        assert_eq!(entry.mtime, None);
        assert_eq!(entry.mode, None);
    }

    #[test]
    fn a_symlink_keeps_its_target() {
        let mut f = file("current", FileType::Symlink);
        f.metadata.symlink = Some(PathBuf::from("/srv/www/releases/7"));

        let entry = convert(&f);
        assert_eq!(entry.kind, EntryKind::Symlink);
        assert_eq!(
            entry.symlink_target,
            Some(PathBuf::from("/srv/www/releases/7"))
        );
    }

    #[test]
    fn errors_map_to_actionable_variants() {
        assert!(matches!(
            translate(
                RemoteError::new(RemoteErrorType::NoSuchFileOrDirectory),
                "listing /nope"
            ),
            VfsError::NotFound(_)
        ));
        assert!(matches!(
            translate(
                RemoteError::new(RemoteErrorType::PermissionDenied),
                "listing /root"
            ),
            VfsError::PermissionDenied(_)
        ));
        assert!(matches!(
            translate(
                RemoteError::new(RemoteErrorType::AuthenticationFailed),
                "connecting"
            ),
            VfsError::AuthFailed(_)
        ));
        assert!(matches!(
            translate(RemoteError::new(RemoteErrorType::NotConnected), "listing"),
            VfsError::NotConnected
        ));
    }

    /// A connection error must keep its reason.
    ///
    /// `remotefs` reports a failed TLS handshake as `ConnectionError` with the
    /// real cause in the source. This used to collapse into the payload-free
    /// `NotConnected`, so a rejected certificate surfaced as "the server
    /// refused the connection" -- which sends you to check a firewall over
    /// something that has nothing to do with one.
    #[test]
    fn a_connection_error_keeps_its_reason() {
        let error = RemoteError::with_source(
            RemoteErrorType::ConnectionError,
            std::io::Error::other("certificate verification failed: unknown issuer"),
        );

        match translate(error, "connecting to box:21") {
            VfsError::Protocol { context, message } => {
                assert_eq!(context, "connecting to box:21");
                assert!(
                    message.contains("certificate"),
                    "the cause has to survive the translation: {message}"
                );
            }
            other => panic!("expected a Protocol error carrying the cause, got {other:?}"),
        }
    }

    #[test]
    fn a_not_found_error_is_recognised_as_such() {
        let error = translate(
            RemoteError::new(RemoteErrorType::NoSuchFileOrDirectory),
            "listing /nope",
        );
        assert!(
            error.is_not_found(),
            "synchronized browsing relies on this to ghost rather than report an error"
        );
    }
}
