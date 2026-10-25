//! How an external tool addresses one side of a sync.
//!
//! `rsync` and `rclone` are separate programs: they cannot use lazysync's
//! [`Vfs`](crate::vfs::Vfs) and have to be handed something they understand on
//! their own command line -- `/home/greg/proj`, `greg@box:/srv/www`,
//! `gdrive:Documents`. This is the translation, and the fact that it can fail
//! is the point: a backend that cannot be named to a tool makes that tool
//! unavailable for the whole plan, which is how `executor = "auto"` decides.
//!
//! **The connection is not reused.** Handing `greg@box:/srv/www` to rsync
//! makes rsync open its own SSH session, with its own authentication, while
//! lazysync's is still open beside it. That is unavoidable -- there is no way
//! to lend a libssh2 session to another process -- and it has a consequence
//! worth stating out loud: a password-authenticated connection cannot be
//! delegated, because rsync would have to prompt for the password again on a
//! terminal lazysync is drawing over. So only key and agent auth qualify; see
//! [`Spec::is_delegable`].

use std::path::{Path, PathBuf};

/// A location as an external tool can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// A path on this machine.
    Local(PathBuf),

    /// An SSH target, for `rsync`'s `user@host:path`.
    Ssh {
        user: Option<String>,
        host: String,
        port: u16,
        /// The key to authenticate with. `None` means the agent, or whatever
        /// `~/.ssh/config` says -- both of which work without a prompt.
        identity: Option<PathBuf>,
        /// Whether lazysync authenticated this session with a typed password.
        ///
        /// Recorded because it disqualifies delegation: rsync would open its
        /// own session and have to ask again, on a terminal the TUI owns.
        password_auth: bool,
        path: PathBuf,
    },

    /// An rclone remote, for `remote:path`.
    Rclone { remote: String, path: PathBuf },
}

impl Spec {
    /// Whether an external tool can be handed this without needing to prompt.
    ///
    /// The whole reason this is a question: a tool that stops for a password
    /// while the TUI holds the alternate screen produces a hung, blank
    /// terminal and no way to see why.
    pub const fn is_delegable(&self) -> bool {
        match self {
            Self::Local(_) | Self::Rclone { .. } => true,
            Self::Ssh { password_auth, .. } => match password_auth {
                true => false,
                false => true,
            },
        }
    }

    /// The argument to put on the tool's command line.
    pub fn argument(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Ssh {
                user, host, path, ..
            } => match user {
                Some(user) => format!("{user}@{host}:{}", path.display()),
                None => format!("{host}:{}", path.display()),
            },
            Self::Rclone { remote, path } => {
                let path = path.display().to_string();
                let trimmed = path.trim_start_matches('/');
                // `remote:` with nothing after it is the remote's own root,
                // which is what an empty path means.
                format!("{remote}:{trimmed}")
            }
        }
    }

    /// The same location with `relative` appended.
    pub fn join(&self, relative: &Path) -> Self {
        match self {
            Self::Local(path) => Self::Local(path.join(relative)),
            Self::Ssh {
                user,
                host,
                port,
                identity,
                password_auth,
                path,
            } => Self::Ssh {
                user: user.clone(),
                host: host.clone(),
                port: *port,
                identity: identity.clone(),
                password_auth: *password_auth,
                path: path.join(relative),
            },
            Self::Rclone { remote, path } => Self::Rclone {
                remote: remote.clone(),
                path: path.join(relative),
            },
        }
    }

    /// The `-e` argument rsync needs to reach this over a non-default port or
    /// with a particular key. `None` when plain `ssh` will do.
    pub fn ssh_transport(&self) -> Option<String> {
        let Self::Ssh { port, identity, .. } = self else {
            return None;
        };

        let mut parts = vec!["ssh".to_string()];
        if *port != 22 {
            parts.push(format!("-p {port}"));
        }
        if let Some(key) = identity {
            // Quoted because a key path may contain spaces, and rsync hands
            // this string to a shell.
            parts.push(format!("-i '{}'", key.display()));
        }

        if parts.len() == 1 {
            None
        } else {
            Some(parts.join(" "))
        }
    }

    pub const fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }

    pub const fn is_ssh(&self) -> bool {
        matches!(self, Self::Ssh { .. })
    }

    pub const fn is_rclone(&self) -> bool {
        matches!(self, Self::Rclone { .. })
    }
}

/// Whether a program is on `PATH`.
///
/// Checked rather than assumed, because `executor = "auto"` has to degrade to
/// the native engine silently and `executor = "rsync"` has to say plainly that
/// rsync is not installed.
pub fn available(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file() && is_executable(&candidate)
        })
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    // On Windows, being on PATH with the right extension is the whole test.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(port: u16, identity: Option<&str>, password_auth: bool) -> Spec {
        Spec::Ssh {
            user: Some("greg".into()),
            host: "box".into(),
            port,
            identity: identity.map(PathBuf::from),
            password_auth,
            path: PathBuf::from("/srv/www"),
        }
    }

    #[test]
    fn a_local_spec_is_just_its_path() {
        assert_eq!(
            Spec::Local("/home/greg/proj".into()).argument(),
            "/home/greg/proj"
        );
    }

    #[test]
    fn an_ssh_spec_is_user_at_host_colon_path() {
        assert_eq!(ssh(22, None, false).argument(), "greg@box:/srv/www");

        let anonymous = Spec::Ssh {
            user: None,
            host: "box".into(),
            port: 22,
            identity: None,
            password_auth: false,
            path: PathBuf::from("/srv"),
        };
        assert_eq!(anonymous.argument(), "box:/srv");
    }

    /// `remote:path`, with no leading slash -- an rclone remote has no root
    /// directory in the filesystem sense, and `gdrive:/Documents` is a
    /// different thing from `gdrive:Documents`.
    #[test]
    fn an_rclone_spec_drops_the_leading_slash() {
        let spec = Spec::Rclone {
            remote: "gdrive".into(),
            path: PathBuf::from("/Documents/notes"),
        };
        assert_eq!(spec.argument(), "gdrive:Documents/notes");

        let root = Spec::Rclone {
            remote: "b2".into(),
            path: PathBuf::from("/"),
        };
        assert_eq!(root.argument(), "b2:");
    }

    /// A default port and no key need no `-e`: passing `ssh` explicitly would
    /// override whatever the user configured in `~/.ssh/config`.
    #[test]
    fn the_default_ssh_transport_is_left_alone() {
        assert_eq!(ssh(22, None, false).ssh_transport(), None);
    }

    #[test]
    fn a_nonstandard_port_and_key_reach_the_transport() {
        let transport = ssh(2222, Some("/home/greg/.ssh/id_ed25519"), false)
            .ssh_transport()
            .expect("a transport");

        assert!(transport.contains("-p 2222"), "{transport}");
        assert!(
            transport.contains("-i '/home/greg/.ssh/id_ed25519'"),
            "{transport}"
        );
    }

    /// The property that keeps the terminal usable: a session lazysync
    /// authenticated with a typed password cannot be handed to rsync, because
    /// rsync would open its own and prompt for it again over the TUI.
    #[test]
    fn a_password_authenticated_session_is_not_delegable() {
        assert!(ssh(22, Some("/key"), false).is_delegable());
        assert!(
            !ssh(22, None, true).is_delegable(),
            "delegating a password session would leave rsync prompting on a screen the TUI \
             is drawing over"
        );
    }

    #[test]
    fn local_and_rclone_are_always_delegable() {
        assert!(Spec::Local("/tmp".into()).is_delegable());
        assert!(Spec::Rclone {
            remote: "b2".into(),
            path: PathBuf::from("/"),
        }
        .is_delegable());
    }

    #[test]
    fn joining_extends_the_path_and_keeps_everything_else() {
        let joined = ssh(2222, Some("/key"), false).join(Path::new("sub/dir"));
        match joined {
            Spec::Ssh {
                path,
                port,
                identity,
                ..
            } => {
                assert_eq!(path, PathBuf::from("/srv/www/sub/dir"));
                assert_eq!(port, 2222, "the port must survive a join");
                assert_eq!(identity, Some(PathBuf::from("/key")));
            }
            other => panic!("expected Ssh, got {other:?}"),
        }
    }

    /// Availability is read off `PATH`, checked against a directory this test
    /// builds rather than against whatever the environment happens to hold.
    ///
    /// It used to assert that `sh` was on `PATH` -- true nearly everywhere,
    /// and so it looked safe. But a test whose premise is "the environment is
    /// normal" fails exactly where testing matters, and this suite already
    /// runs inside a Nix sandbox with a deliberately sparse `PATH`.
    #[test]
    fn availability_is_checked_against_path() {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().unwrap();

        let program = dir.path().join("lazysync-test-tool");
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();

        // Present on PATH but not executable, which is not the same as
        // usable -- so the check has to look at the mode, not just the name.
        let not_executable = dir.path().join("lazysync-test-not-exec");
        std::fs::write(&not_executable, b"data").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&not_executable, std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }

        let previous = std::env::var_os("PATH");
        // SAFETY: the mutex above makes this the only thread touching PATH,
        // and it is restored below before the guard drops.
        unsafe { std::env::set_var("PATH", dir.path()) };

        let found = available("lazysync-test-tool");
        let missing = available("lazysync-definitely-not-a-real-program");
        let unexecutable = available("lazysync-test-not-exec");

        match previous {
            Some(value) => unsafe { std::env::set_var("PATH", value) },
            None => unsafe { std::env::remove_var("PATH") },
        }

        assert!(found, "an executable on PATH should be found");
        assert!(!missing, "a name that is not there should not be");
        #[cfg(unix)]
        assert!(
            !unexecutable,
            "a non-executable file on PATH is not a usable program"
        );
    }
}
