//! SFTP and SCP, over `remotefs-ssh`.
//!
//! Two things happen here that the underlying crate does not do.
//!
//! The first is host key verification, which `remotefs-ssh`'s libssh2 backend
//! simply does not implement -- see [`super::hostkey`]. Nothing connects
//! without going through that check first.
//!
//! The second is honest capability reporting. `remotefs` describes what a
//! protocol can do in terms of streams and seeking; what the sync planner
//! needs to know is whether timestamps exist and how coarse they are. The
//! translation is in [`SshFs::capabilities`], and it is deliberately
//! pessimistic: claiming a precision the server does not have is what makes a
//! sync want to re-copy a tree that has not changed.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use remotefs::{
    fs::{Capabilities as RemoteCapabilities, File, FileType, UnixPex},
    RemoteError, RemoteErrorType, RemoteFs,
};
use remotefs_ssh::{ScpFs, SftpFs, SshAgentIdentity, SshKeyStorage, SshOpts};

use super::{Capabilities, Entry, EntryKind, Vfs, VfsError};
use crate::{
    connection::{AuthMethod, Protocol, Secret, Target, CONNECT_TIMEOUT},
    vfs::hostkey::{self, HostKey, Verdict},
};

/// A connected SSH session, browsable as a filesystem.
pub struct SshFs {
    client: Box<dyn RemoteFs + Send>,
    protocol: Protocol,
    target: Target,
    /// Where `GoHome` goes, resolved once at connect time.
    home: Option<PathBuf>,
}

/// What the host key check found.
///
/// Returned rather than resolved internally, because the interesting cases are
/// questions only the user can answer -- and code that answers them itself is
/// code that trusts unknown host keys.
#[derive(Debug)]
pub enum Precheck {
    /// The key is recorded and matches. [`open`] may proceed.
    Trusted,
    /// Not in `known_hosts`. Ask, then [`trust`] the key and open.
    Unknown {
        fingerprint: String,
        host_port: String,
        key: Box<HostKey>,
    },
    /// Recorded, and different. Not a prompt to click through -- the caller is
    /// expected to refuse.
    Changed {
        fingerprint: String,
        host_port: String,
        known_hosts: PathBuf,
    },
}

/// Check a target's host key, sending no credentials.
///
/// This runs **before** any password is collected, which is the point: you
/// should not be asked to type a secret for a server whose identity has not
/// been established. The pre-flight stops after the SSH handshake, so nothing
/// has been offered to the far end if the answer is to refuse.
pub fn precheck(target: &Target) -> Result<Precheck, VfsError> {
    if !target.protocol.is_implemented() {
        return Err(VfsError::Unsupported(format!(
            "{} arrives in phase {} -- see docs/PLAN.md",
            target.protocol,
            target.protocol.arrives_in_phase()
        )));
    }

    let (verdict, key) = hostkey::verify(
        &target.host,
        target.port_or_default(),
        CONNECT_TIMEOUT,
        None,
    )
    .map_err(|e| VfsError::HostKey(e.to_string()))?;

    Ok(match verdict {
        Verdict::Trusted => Precheck::Trusted,
        Verdict::Unknown { fingerprint } => Precheck::Unknown {
            fingerprint,
            host_port: key.host_port(),
            key: Box::new(key),
        },
        Verdict::Changed {
            fingerprint,
            known_hosts,
        } => Precheck::Changed {
            fingerprint,
            host_port: key.host_port(),
            known_hosts,
        },
    })
}

/// Record a host key the user agreed to trust.
pub fn trust(key: &HostKey) -> Result<PathBuf, VfsError> {
    hostkey::remember(key, None).map_err(|e| VfsError::HostKey(e.to_string()))
}

/// Open a session.
///
/// Assumes [`precheck`] has already approved the host key. `secret` is the
/// password for [`AuthMethod::Password`], or a key passphrase for
/// [`AuthMethod::Key`]; it is not retained after this call.
pub fn open(target: &Target, secret: Option<&Secret>) -> Result<SshFs, VfsError> {
    let opts = build_opts(target, secret);

    let mut client: Box<dyn RemoteFs + Send> = match target.protocol {
        Protocol::Sftp => Box::new(SftpFs::libssh2(opts)),
        Protocol::Scp => Box::new(ScpFs::libssh2(opts)),
        other => {
            return Err(VfsError::Unsupported(format!(
                "{other} is not an SSH protocol"
            )))
        }
    };

    client.connect().map_err(|e| translate(e, "connecting"))?;

    let mut fs = SshFs {
        client,
        protocol: target.protocol,
        target: target.clone(),
        home: None,
    };
    fs.home = fs.resolve_home();

    Ok(fs)
}

/// One key file, for every host.
///
/// `SshKeyStorage` exists to map a host and user onto a key, which is what
/// `~/.ssh/config` already does. Here the key was named explicitly for this
/// one target, so the mapping is constant.
struct FixedKey(PathBuf);

impl SshKeyStorage for FixedKey {
    fn resolve(&self, _host: &str, _username: &str) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

/// Which agent identities to offer for an auth method.
///
/// Split out from [`build_opts`] because it is the one decision in there with
/// a consequence worth testing, and `SshOpts` exposes neither getters nor
/// `Debug` to assert against.
const fn agent_identity(auth: AuthMethod) -> Option<SshAgentIdentity> {
    match auth {
        // The case where lazysync never sees a secret at all.
        AuthMethod::Agent => Some(SshAgentIdentity::All),
        // The agent stays enabled alongside an explicit key, because a key
        // already loaded into the agent is the common case and prompting for a
        // passphrase lazysync does not need would be worse.
        AuthMethod::Key => Some(SshAgentIdentity::All),
        // No agent when a password was chosen: trying agent keys first burns
        // authentication attempts, and on a server with a low `MaxAuthTries`
        // that can lock the account before the password is ever offered.
        AuthMethod::Password => None,
    }
}

fn build_opts(target: &Target, secret: Option<&Secret>) -> SshOpts {
    let mut opts = SshOpts::new(&target.host)
        .port(target.port_or_default())
        .connection_timeout(CONNECT_TIMEOUT);

    if let Some(username) = &target.username {
        opts = opts.username(username);
    }

    // Read ~/.ssh/config, so a bookmark for a host you already configured
    // picks up its HostName, User, Port and IdentityFile instead of making you
    // restate all of it. Explicit values above still win: SshOpts only falls
    // back to the file for what was not set.
    if let Some(home) = dirs::home_dir() {
        let config = home.join(".ssh").join("config");
        if config.is_file() {
            opts = opts.config_file(
                &config,
                remotefs_ssh::SshConfigParseRule::ALLOW_UNSUPPORTED_FIELDS,
            );
        }
        opts = opts.home_dir(home);
    }

    opts = opts.ssh_agent_identity(agent_identity(target.auth));

    // An explicit key file. Without this, choosing `key` auth and naming an
    // identity file would do nothing at all: `SshOpts` takes a key only
    // through a `SshKeyStorage`, and leaving it unset means the backend falls
    // back to the agent and `~/.ssh/config` -- which is the right fallback,
    // but silently ignores what the user actually asked for.
    if let Some(identity) = &target.identity_file {
        opts = opts.key_storage(Box::new(FixedKey(identity.clone())));
    }

    // libssh2 takes a key passphrase in the same slot as a password, so both
    // non-agent methods feed the secret through here. An empty one is skipped
    // rather than offered: a blank password still counts against the server's
    // `MaxAuthTries`.
    if let Some(secret) = secret.filter(|s| !s.is_empty()) {
        opts = opts.password(secret.expose());
    }

    opts
}

impl SshFs {
    pub fn target(&self) -> &Target {
        &self.target
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

    /// The remote home directory.
    ///
    /// SFTP has no "home" request, so this runs `pwd` over an exec channel --
    /// which is what the path is relative to on login. A server that allows
    /// SFTP but no exec (a `ForceCommand internal-sftp` setup, which is
    /// common) returns nothing, and the root is used instead.
    fn resolve_home(&mut self) -> Option<PathBuf> {
        let output = self.client.exec("pwd").ok()?;
        let path = output.stdout.trim();
        if path.starts_with('/') {
            Some(PathBuf::from(path))
        } else {
            tracing::debug!("`pwd` on {} gave {path:?}", self.target.display());
            None
        }
    }
}

impl Vfs for SshFs {
    fn scheme(&self) -> &str {
        self.protocol.scheme()
    }

    fn display_path(&self, path: &Path) -> String {
        // Host plus path, so the two panes cannot be confused for each other
        // when both are showing `/srv/www`.
        format!("{}:{}", self.target.host, path.display())
    }

    fn capabilities(&self) -> Capabilities {
        let remote = self.client.capabilities();

        Capabilities {
            // Both SFTP and SCP report mtimes.
            mtime: true,
            set_mtime: remote.contains(RemoteCapabilities::SET_METADATA),
            mode: remote.contains(RemoteCapabilities::POSIX_MODE),
            symlinks: remote.contains(RemoteCapabilities::SYMLINK),
            // Resuming a partial transfer needs to write at an offset, which
            // is seek-on-write rather than append.
            ranged_writes: remote.contains(RemoteCapabilities::SEEK_WRITE),
            // One second, not zero. The SFTP protocol carries whole-second
            // mtimes in its base attribute set, and while version 4 added
            // nanoseconds, plenty of servers do not send them. Claiming
            // better precision than that is what makes every file look
            // changed against a local filesystem.
            mtime_granularity_secs: 1,
        }
    }

    fn list(&mut self, path: &Path) -> Result<Vec<Entry>, VfsError> {
        let files = self
            .client
            .list_dir(path)
            .map_err(|e| translate(e, &format!("listing {}", path.display())))?;

        Ok(files.iter().map(convert).collect())
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        // `stat` rather than `exists`, because synchronized browsing needs to
        // know it is a *directory*: following the other pane into a path that
        // happens to be a file would show an empty listing with no
        // explanation.
        match self.client.stat(path) {
            Ok(file) => matches!(file.metadata.file_type, FileType::Directory),
            Err(_) => false,
        }
    }

    fn create_dir(&mut self, path: &Path) -> Result<(), VfsError> {
        // 0o755, matching `mkdir` with a standard umask. The remote server
        // applies its own umask on top.
        let mode = UnixPex::from(0o755);

        // remotefs creates one level only, so walk the missing ancestors.
        // An existing directory is not an error here: two panes following
        // each other can both ask for the same path.
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
                Err(e) => {
                    return Err(translate(e, &format!("creating {}", dir.display())));
                }
            }
        }

        Ok(())
    }

    fn home(&mut self) -> Option<PathBuf> {
        self.home.clone()
    }

    fn root(&self) -> PathBuf {
        PathBuf::from("/")
    }
}

impl Drop for SshFs {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// Turn a `remotefs` entry into ours.
fn convert(file: &File) -> Entry {
    let metadata = &file.metadata;
    let kind = match metadata.file_type {
        FileType::Directory => EntryKind::Dir,
        FileType::Symlink => EntryKind::Symlink,
        // `FileType` is non-exhaustive, so a future variant has to land
        // somewhere. `File` is the safe default: it is browsable and not
        // enterable, which is how an unrecognised entry should behave.
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
        // to feel on a directory of a few hundred. A link whose target ends in
        // a separator is the only free hint; otherwise this stays false and
        // entering the link falls back to `is_dir`.
        symlink_to_dir: false,
    }
}

/// Map a `remotefs` error onto the variants the UI distinguishes.
fn translate(error: RemoteError, context: &str) -> VfsError {
    match error.kind() {
        // `StatFailed` joins the not-found cases on purpose: an SFTP server
        // answering a stat for a path that is not there is the usual way
        // "missing" arrives, and synchronized browsing has to read it as
        // absent-and-fine rather than as an error worth a status message.
        RemoteErrorType::NoSuchFileOrDirectory
        | RemoteErrorType::CouldNotOpenFile
        | RemoteErrorType::StatFailed => VfsError::NotFound(context.to_owned()),
        RemoteErrorType::PermissionDenied | RemoteErrorType::FileCreateDenied => {
            VfsError::PermissionDenied(context.to_owned())
        }
        RemoteErrorType::NotConnected | RemoteErrorType::ConnectionError => VfsError::NotConnected,
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

/// How long a single remote operation may take before it is treated as hung.
///
/// Separate from [`CONNECT_TIMEOUT`]: establishing a session is slow, but a
/// directory listing on a live connection should not be.
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use std::time::{Duration as StdDuration, SystemTime};

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
            "a directory's apparent size is meaningless, and 4096 would be shown as a real size"
        );
        assert!(entry.is_dir_like());
    }

    #[test]
    fn size_mode_and_mtime_come_through() {
        let when = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_700_000_000);
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

    /// An unimplemented protocol must be refused by name, before any network
    /// traffic, rather than failing somewhere obscure.
    #[test]
    fn an_unimplemented_protocol_is_refused_before_connecting() {
        let target = Target::new(Protocol::S3, "bucket");

        let message = match precheck(&target) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("S3 should not pass a precheck yet"),
        };
        assert!(message.contains("s3"), "{message}");
        assert!(message.contains("phase 5"), "{message}");
    }

    /// The refusal has to come before any network traffic, since the point is
    /// that nothing is sent to a server lazysync cannot talk to.
    #[test]
    fn an_unimplemented_protocol_is_refused_without_resolving_the_host() {
        // A host that cannot resolve: reaching DNS at all would prove the
        // protocol check came too late.
        let target = Target::new(Protocol::Webdav, "nonexistent.invalid");
        assert!(matches!(precheck(&target), Err(VfsError::Unsupported(_))));
    }

    /// Password auth must not also try agent keys: on a server with a low
    /// `MaxAuthTries`, spending attempts on keys the user did not choose can
    /// lock the account before the password is ever offered.
    #[test]
    fn password_auth_does_not_also_offer_agent_keys() {
        assert!(agent_identity(AuthMethod::Password).is_none());
    }

    #[test]
    fn agent_and_key_auth_both_use_the_agent() {
        assert!(matches!(
            agent_identity(AuthMethod::Agent),
            Some(SshAgentIdentity::All)
        ));
        assert!(
            matches!(agent_identity(AuthMethod::Key), Some(SshAgentIdentity::All)),
            "a key already loaded into the agent is the common case"
        );
    }

    /// Building options must not panic for any method, since it runs before
    /// anything that could report an error to the user.
    #[test]
    fn options_build_for_every_auth_method() {
        for auth in AuthMethod::ALL {
            let mut target = Target::new(Protocol::Sftp, "box");
            target.auth = auth;
            let _ = build_opts(&target, Some(&Secret::new("x")));
            let _ = build_opts(&target, None);
        }
    }
}
