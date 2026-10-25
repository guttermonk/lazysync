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

use std::path::PathBuf;

use remotefs::RemoteFs;
use remotefs_ssh::{ScpFs, SftpFs, SshAgentIdentity, SshKeyStorage, SshOpts};

use super::{remote::RemoteAdapter, VfsError};
use crate::{
    connection::{AuthMethod, Protocol, Secret, Target, CONNECT_TIMEOUT},
    vfs::hostkey::{self, HostKey, Verdict},
};

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
pub fn open(target: &Target, secret: Option<&Secret>) -> Result<RemoteAdapter, VfsError> {
    let opts = build_opts(target, secret);

    let client: Box<dyn RemoteFs + Send> = match target.protocol {
        Protocol::Sftp => Box::new(SftpFs::libssh2(opts)),
        Protocol::Scp => Box::new(ScpFs::libssh2(opts)),
        other => {
            return Err(VfsError::Unsupported(format!(
                "{other} is not an SSH protocol"
            )))
        }
    };

    let mut adapter = RemoteAdapter::new(client, target.protocol, target.clone(), None);
    adapter.connect_now()?;

    // SFTP has no "home" request, so this runs `pwd` over an exec channel --
    // which is what the path is relative to on login. A server that allows
    // SFTP but no exec (`ForceCommand internal-sftp`, which is common)
    // answers nothing, and the root is used instead.
    let home = adapter.probe_home_via_shell_pwd();
    adapter.set_home(home);

    Ok(adapter)
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(message.contains("phase 6"), "{message}");
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

    /// FTP is implemented but has no host key, so it must not be routed
    /// through the SSH precheck -- which would reject it as "not an SSH
    /// protocol" and make FTP unusable.
    #[test]
    fn ftp_is_implemented_but_is_not_an_ssh_protocol() {
        assert!(Protocol::Ftp.is_implemented());
        assert!(!Protocol::Ftp.is_ssh());
        assert!(Protocol::Sftp.is_ssh());
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

    /// SFTP's rename refuses an existing target, which is why the executor has
    /// a swap path at all. Pinned here because flipping it would silently
    /// break every overwrite.
    #[test]
    fn ssh_does_not_claim_an_overwriting_rename() {
        for protocol in [Protocol::Sftp, Protocol::Scp] {
            let caps = protocol.capabilities();
            assert!(!caps.atomic_overwrite_rename);
            assert!(caps.mtime && caps.set_mtime && caps.mode);
        }
    }
}
