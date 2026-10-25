//! What to connect to, and how.
//!
//! A [`Target`] is the protocol-independent description of one side of a
//! session: where it is, who you are, and which path to open. Both a URL typed
//! on the command line and a saved bookmark resolve to one, which is why the
//! bookmark screen and `lazysync sftp://...` share a single connect path.
//!
//! Credentials are deliberately *not* part of a `Target`. A target is
//! serialized into `bookmarks.toml`; a password is not. Keeping them in
//! separate types means the thing that gets written to disk cannot carry a
//! secret by accident.

pub mod url;

use std::{fmt, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

pub use url::{parse, ParseTargetError};

/// Which protocol a target speaks.
///
/// SFTP, SCP, FTP and FTPS are implemented. The rest are listed because a bookmark
/// file written against the documented schema should load, and because the
/// connection form has to offer something honest when you pick one -- see
/// [`Protocol::is_implemented`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// SFTP over SSH. The default, and what you almost always want.
    Sftp,
    /// SCP over SSH. Faster for a single large file, but it cannot list a
    /// directory without a shell, so browsing is less reliable.
    Scp,
    Ftp,
    Ftps,
    S3,
    Smb,
    Webdav,
    /// Any remote in the user's `rclone.conf`, reached by running `rclone`.
    ///
    /// The host field holds the remote's name, so `rclone://gdrive/Documents`
    /// is the remote `gdrive` at path `/Documents`. This is how S3, WebDAV,
    /// B2, Google Drive and some seventy others are reachable without a
    /// `remotefs` backend for each -- see [`crate::vfs::rclone`].
    Rclone,
}

impl Protocol {
    pub const ALL: [Self; 8] = [
        Self::Sftp,
        Self::Scp,
        Self::Ftp,
        Self::Ftps,
        Self::Rclone,
        Self::S3,
        Self::Smb,
        Self::Webdav,
    ];

    /// The URL scheme and the name used in `bookmarks.toml`.
    pub const fn scheme(self) -> &'static str {
        match self {
            Self::Sftp => "sftp",
            Self::Scp => "scp",
            Self::Ftp => "ftp",
            Self::Ftps => "ftps",
            Self::S3 => "s3",
            Self::Smb => "smb",
            Self::Webdav => "webdav",
            Self::Rclone => "rclone",
        }
    }

    pub fn from_scheme(scheme: &str) -> Option<Self> {
        let lower = scheme.to_ascii_lowercase();
        Self::ALL.into_iter().find(|p| p.scheme() == lower)
    }

    /// The port used when a target does not name one.
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Sftp | Self::Scp => 22,
            // 21 for FTPS too, not 990. 990 is *implicit* FTPS, where TLS
            // starts before a byte of FTP is spoken; what lazysync does is
            // *explicit* FTPS -- connect in the clear on the ordinary port and
            // send `AUTH TLS` -- which is what RFC 4217 specifies and what
            // `suppaftp`'s `into_secure` performs. Defaulting to 990 would
            // make `ftps://host` open a plaintext connection to a port that is
            // waiting for a TLS handshake, and hang.
            Self::Ftp | Self::Ftps => 21,
            Self::S3 | Self::Webdav => 443,
            Self::Smb => 445,
            // No port: rclone runs as a subprocess and the remote's own
            // config says how to reach wherever it points.
            Self::Rclone => 0,
        }
    }

    /// Whether lazysync can actually connect with this protocol yet.
    ///
    /// The form and the bookmark store accept all of them, so that a config
    /// written for a later version still loads; the connect path refuses the
    /// unimplemented ones by name instead of failing obscurely.
    pub const fn is_implemented(self) -> bool {
        matches!(
            self,
            Self::Sftp | Self::Scp | Self::Ftp | Self::Ftps | Self::Rclone
        )
    }

    /// What is standing in the way, for the message shown when you pick an
    /// unimplemented protocol. `None` for the ones that work.
    ///
    /// A reason rather than a phase number. A phase number is a promise, and
    /// these three have already slipped two phases: each needs something
    /// lazysync does not have, and naming the obstacle is both more useful to
    /// whoever picked the protocol and incapable of going stale.
    pub const fn unavailable_because(self) -> Option<&'static str> {
        match self {
            Self::Sftp | Self::Scp | Self::Ftp | Self::Ftps | Self::Rclone => None,

            // `remotefs-webdav` also still wants `remotefs 0.3`, whose
            // `RemoteFs` is a different and incompatible trait -- so it cannot
            // share the adapter every other backend uses.
            Self::Webdav => Some(
                "its client needs a tokio runtime, and lazysync deliberately has none; where \
                 that runtime would live is a decision in its own right",
            ),
            // Same trait-version problem, plus schema work.
            Self::S3 => Some(
                "it needs bucket, region, endpoint and access-key fields, which neither the \
                 bookmark schema nor the connection form has",
            ),
            Self::Smb => Some(
                "it needs a share name, and libsmbclient as a system dependency in the Nix \
                 build -- or reach it as an rclone remote instead",
            ),
        }
    }

    /// Whether lazysync has to collect a credential for this protocol.
    ///
    /// rclone does not: its own `rclone.conf` holds whatever the remote needs,
    /// including OAuth tokens it refreshes itself. Prompting for a password
    /// would be asking for something lazysync has nowhere to put -- and the
    /// keyring entry it created would never be read.
    pub const fn needs_credentials(self) -> bool {
        !matches!(self, Self::Rclone)
    }

    /// What a backend of this protocol can do.
    ///
    /// Declared per protocol rather than read off `remotefs`'s own capability
    /// flags, because those describe streams and seeking while the planner
    /// asks about timestamps and how precise they are. Getting one wrong is
    /// how a sync re-copies a tree that has not changed, so each is stated
    /// deliberately and pessimistically.
    pub const fn capabilities(self) -> crate::vfs::Capabilities {
        use crate::vfs::Capabilities;

        match self {
            // SFTP and SCP: full Unix semantics. The protocol's base attribute
            // set carries whole-second mtimes; version 4 added nanoseconds and
            // plenty of servers do not send them, so 1 second is the honest
            // floor.
            Self::Sftp | Self::Scp => Capabilities {
                mtime: true,
                set_mtime: true,
                mode: true,
                symlinks: true,
                ranged_writes: true,
                // SFTP's rename refuses an existing target: OpenSSH answers
                // SSH_FX_PERMISSION_DENIED rather than replacing it.
                atomic_overwrite_rename: false,
                mtime_granularity_secs: 1,
            },

            // FTP: `MDTM` gives whole seconds and `MFMT` can set them, but
            // neither is required by the protocol and plenty of servers lack
            // MFMT. Claiming `set_mtime` would mean every sync re-copying the
            // tree on a server that silently ignores it, so this says no and
            // the plan carries the caveat.
            Self::Ftp | Self::Ftps => Capabilities {
                mtime: true,
                set_mtime: false,
                mode: false,
                symlinks: false,
                // REST before STOR is widely supported, which is what resuming
                // an upload needs.
                ranged_writes: true,
                atomic_overwrite_rename: false,
                // A *day*, which is not a typo. Listings come from `LIST`,
                // whose format is the output of `ls -l`: a recent file shows
                // `Oct 24 22:52`, losing the seconds, and a file older than
                // about six months shows `Nov 14  2023` -- losing the time
                // altogether. Measured against a real server, the error was
                // 52 seconds on a file from today and **80,017 seconds** on
                // one from 2023.
                //
                // Claiming 1 second here would make a pull from FTP see an
                // identical tree as entirely changed, since every remote
                // timestamp sits up to a day below its local counterpart.
                // `MDTM` would give real seconds but costs a round trip per
                // file, so finer comparison is a deliberate future choice
                // rather than something to assume here.
                mtime_granularity_secs: 86_400,
            },

            // WebDAV: `getlastmodified` is an HTTP date, so whole seconds, and
            // there is no standard way to set one. No modes, no symlinks.
            Self::Webdav => Capabilities {
                mtime: true,
                set_mtime: false,
                mode: false,
                symlinks: false,
                ranged_writes: false,
                atomic_overwrite_rename: true,
                mtime_granularity_secs: 1,
            },

            // S3 has no modification time of its own -- the planner falls back
            // to size, which is why `size_mtime` degrades rather than
            // reporting every object as changed.
            Self::S3 => Capabilities {
                mtime: false,
                set_mtime: false,
                mode: false,
                symlinks: false,
                ranged_writes: false,
                atomic_overwrite_rename: true,
                mtime_granularity_secs: 1,
            },

            // rclone: whatever is behind the remote. Declared pessimistically
            // because one remote name could be S3 or a local disk, and the
            // comparison rules must hold for the worst of them. `RcloneFs`
            // reports the same thing; this exists for the form and the
            // bookmark list, which have no live backend to ask.
            Self::Rclone => Capabilities {
                mtime: true,
                set_mtime: false,
                mode: false,
                symlinks: false,
                ranged_writes: false,
                atomic_overwrite_rename: true,
                mtime_granularity_secs: 1,
            },

            // SMB: timestamps and a rename that replaces, but no Unix modes
            // over the wire.
            Self::Smb => Capabilities {
                mtime: true,
                set_mtime: true,
                mode: false,
                symlinks: false,
                ranged_writes: true,
                atomic_overwrite_rename: true,
                mtime_granularity_secs: 1,
            },
        }
    }

    /// Whether this protocol authenticates with an SSH key or agent, which is
    /// what lets the password prompt be skipped entirely.
    pub const fn is_ssh(self) -> bool {
        matches!(self, Self::Sftp | Self::Scp)
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.scheme())
    }
}

/// How to prove who you are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// Whatever `ssh-agent` is holding. The default, because it is the case
    /// where lazysync never has to see a secret at all.
    #[default]
    Agent,
    /// A private key file, which may itself have a passphrase.
    Key,
    /// A password typed at the prompt, or taken from the keyring.
    Password,
}

impl AuthMethod {
    pub const ALL: [Self; 3] = [Self::Agent, Self::Key, Self::Password];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Key => "key",
            Self::Password => "password",
        }
    }

    /// Whether a secret has to be collected before connecting.
    ///
    /// Agent auth needs nothing. Key auth usually needs nothing -- an
    /// unencrypted key, or one already loaded into the agent -- so lazysync
    /// tries without a passphrase first and only asks if that fails.
    pub const fn needs_secret_upfront(self) -> bool {
        matches!(self, Self::Password)
    }
}

impl fmt::Display for AuthMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Everything about one side of a session except the secret.
///
/// This is what `bookmarks.toml` stores and what the connection form edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub protocol: Protocol,
    pub host: String,
    /// `None` means [`Protocol::default_port`], so a bookmark does not record
    /// a port that was never chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// `None` defers to `~/.ssh/config`, then to the local username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default)]
    pub auth: AuthMethod,
    /// Private key file for [`AuthMethod::Key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<PathBuf>,
    /// Directory to open in the remote pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<PathBuf>,
}

impl Target {
    pub fn new(protocol: Protocol, host: impl Into<String>) -> Self {
        Self {
            protocol,
            host: host.into(),
            port: None,
            username: None,
            auth: AuthMethod::default(),
            identity_file: None,
            remote_path: None,
        }
    }

    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or_else(|| self.protocol.default_port())
    }

    /// `sftp://greg@box:2222/srv/www`, for titles and log lines.
    ///
    /// Never includes a secret: there is nowhere in this type for one to be.
    pub fn display(&self) -> String {
        let mut out = format!("{}://", self.protocol);
        if let Some(user) = &self.username {
            out.push_str(user);
            out.push('@');
        }
        out.push_str(&self.host);
        if let Some(port) = self.port {
            if port != self.protocol.default_port() {
                out.push(':');
                out.push_str(&port.to_string());
            }
        }
        if let Some(path) = &self.remote_path {
            out.push_str(&path.display().to_string());
        }
        out
    }

    /// `host:port`, the key host keys are recorded against.
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.host, self.port_or_default())
    }
}

/// A secret, held only as long as it takes to connect.
///
/// Not `Debug`-printable and not serializable, so it cannot reach a log line
/// or `bookmarks.toml` by accident. `Drop` zeroes the bytes, which is best
/// effort -- it does not undo a reallocation the `String` may have made -- but
/// it keeps the obvious copy from lingering in freed memory.
#[derive(Clone, Default)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately opaque: a `{:?}` in a tracing call must not leak it.
        f.write_str(if self.0.is_empty() {
            "Secret(empty)"
        } else {
            "Secret(***)"
        })
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY-adjacent note: overwriting the bytes in place rather than
        // just dropping, so the password is not left in freed heap memory.
        unsafe {
            for byte in self.0.as_bytes_mut() {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

/// How long to wait for a connection before giving up.
///
/// Short on purpose. The alternative is a UI that appears to hang with no way
/// out, which is worse than an error you can retry -- and the blocking `Vfs`
/// means a stuck connect blocks the draw loop.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ports_match_the_protocols() {
        assert_eq!(Protocol::Sftp.default_port(), 22);
        assert_eq!(Protocol::Scp.default_port(), 22);
        assert_eq!(Protocol::Ftp.default_port(), 21);
        assert_eq!(Protocol::Smb.default_port(), 445);
    }

    #[test]
    fn schemes_round_trip() {
        for protocol in Protocol::ALL {
            assert_eq!(Protocol::from_scheme(protocol.scheme()), Some(protocol));
        }
        assert_eq!(Protocol::from_scheme("SFTP"), Some(Protocol::Sftp));
        assert_eq!(Protocol::from_scheme("gopher"), None);
    }

    #[test]
    fn the_implemented_protocols_are_ssh_and_ftp() {
        for protocol in [Protocol::Sftp, Protocol::Scp, Protocol::Ftp, Protocol::Ftps] {
            assert!(protocol.is_implemented(), "{protocol}");
        }
        for protocol in [Protocol::S3, Protocol::Smb, Protocol::Webdav] {
            assert!(!protocol.is_implemented(), "{protocol}");
            // An unimplemented protocol has to say what is blocking it. A
            // phase number would be a promise, and these have already slipped
            // two; the obstacle is both more useful and cannot go stale.
            assert!(
                protocol.unavailable_because().is_some(),
                "{protocol} is refused without saying why"
            );
        }
        for protocol in [Protocol::Sftp, Protocol::Scp, Protocol::Ftp, Protocol::Ftps] {
            assert!(
                protocol.unavailable_because().is_none(),
                "{protocol} works, so it must not claim an obstacle"
            );
        }
    }

    /// Capabilities drive the planner's comparison rules, so every protocol
    /// has to state them -- and the ones that cannot set a timestamp have to
    /// say so, or a sync against them re-copies its whole tree every run with
    /// no explanation on screen.
    #[test]
    fn every_protocol_declares_capabilities_and_none_overclaims() {
        for protocol in Protocol::ALL {
            let caps = protocol.capabilities();
            assert!(
                caps.mtime_granularity_secs >= 1,
                "{protocol}: claiming sub-second precision no protocol carries is how a sync \
                 decides every file changed"
            );
            assert!(
                !(caps.set_mtime && !caps.mtime),
                "{protocol}: setting a timestamp it cannot read makes no sense"
            );
        }

        // The specific admissions, pinned.
        assert!(!Protocol::Ftp.capabilities().set_mtime);
        assert!(!Protocol::Webdav.capabilities().set_mtime);
        assert!(
            !Protocol::S3.capabilities().mtime,
            "S3 has no modification time of its own; the planner falls back to size"
        );
        assert!(
            !Protocol::Sftp.capabilities().atomic_overwrite_rename,
            "SFTP's rename refuses an existing target"
        );
    }

    #[test]
    fn display_omits_a_default_port_and_includes_a_custom_one() {
        let mut target = Target::new(Protocol::Sftp, "box");
        target.username = Some("greg".into());
        target.remote_path = Some(PathBuf::from("/srv/www"));

        assert_eq!(target.display(), "sftp://greg@box/srv/www");

        target.port = Some(22);
        assert_eq!(
            target.display(),
            "sftp://greg@box/srv/www",
            "the default port is noise"
        );

        target.port = Some(2222);
        assert_eq!(target.display(), "sftp://greg@box:2222/srv/www");
    }

    #[test]
    fn host_port_falls_back_to_the_protocol_default() {
        let target = Target::new(Protocol::Sftp, "box");
        assert_eq!(target.host_port(), "box:22");
    }

    /// A bookmark file must never grow a password field, so the serialized
    /// form is checked rather than assumed.
    #[test]
    fn a_serialized_target_carries_no_secret() {
        let mut target = Target::new(Protocol::Sftp, "box");
        target.username = Some("greg".into());
        target.auth = AuthMethod::Password;

        let rendered = toml::to_string(&target).unwrap();

        // Per line, not as a substring: the value `auth = "password"` is
        // legitimate, and only a *field* named password or secret is not.
        let secret_field = rendered
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("password") || line.starts_with("secret"));
        assert_eq!(secret_field, None, "{rendered}");

        assert!(rendered.contains(r#"auth = "password""#), "{rendered}");
    }

    /// Unset optionals must not be written, or every bookmark gains a
    /// `port = ` line for a port nobody chose.
    #[test]
    fn unset_fields_are_omitted_from_the_serialized_form() {
        let rendered = toml::to_string(&Target::new(Protocol::Sftp, "box")).unwrap();
        assert!(!rendered.contains("port"), "{rendered}");
        assert!(!rendered.contains("username"), "{rendered}");
        assert!(!rendered.contains("identity_file"), "{rendered}");
    }

    #[test]
    fn a_secret_does_not_print_itself() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert_eq!(format!("{:?}", Secret::default()), "Secret(empty)");
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn agent_auth_needs_no_prompt() {
        assert!(!AuthMethod::Agent.needs_secret_upfront());
        assert!(
            !AuthMethod::Key.needs_secret_upfront(),
            "an unencrypted key or one in the agent should connect without asking"
        );
        assert!(AuthMethod::Password.needs_secret_upfront());
    }
}
