//! SFTP against a real OpenSSH server.
//!
//! A protocol backend tested only against a mock agrees with the mock and
//! nothing else, so these run against an actual `sshd` started by
//! `tests/fixtures/sshd.sh` -- its own host key, its own `authorized_keys`,
//! an isolated `HOME`, loopback only, on a high port, as the invoking user.
//!
//! They are skipped when the fixture cannot run (no `sshd`, no `ssh-keygen`),
//! rather than failing: a contributor without OpenSSH installed should still
//! get a green `cargo test`. The Nix dev shell provides both, so the skip does
//! not quietly hide a regression for anyone following CONTRIBUTING.md.
//!
//! `HOME` is overridden for the duration, because host key verification reads
//! and writes `~/.ssh/known_hosts` and a test must not touch the real one.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use lazysync::{
    connection::{AuthMethod, Protocol, Target},
    vfs::{
        hostkey::{self, Verdict},
        ssh, EntryKind, Vfs, VfsError,
    },
};

/// A running fixture, torn down on drop.
struct Server {
    work: PathBuf,
    port: u16,
    user: String,
    home: PathBuf,
    served: PathBuf,
}

impl Server {
    /// Start one, or `None` if the fixture cannot run here.
    fn start() -> Option<Self> {
        if which("sshd").is_none() && !Path::new("/usr/sbin/sshd").exists() {
            eprintln!("skipping: no sshd");
            return None;
        }
        if which("ssh-keygen").is_none() {
            eprintln!("skipping: no ssh-keygen");
            return None;
        }

        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sshd.sh");

        // A directory per *server*, not per process. Cargo runs these tests in
        // parallel within one binary, and the fixture script starts by
        // `rm -rf`ing its work directory -- so a shared path means one test
        // deletes another's sshd config and key out from under it.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let work = std::env::temp_dir().join(format!(
            "lazysync-sshd-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));

        let output = Command::new("bash")
            .arg(&script)
            .arg("start")
            .arg(&work)
            .output()
            .ok()?;

        if !output.status.success() {
            eprintln!(
                "skipping: the sshd fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }

        // The script emits `export NAME=value` lines.
        let exports: HashMap<String, String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("export "))
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();

        Some(Self {
            port: exports.get("LAZYSYNC_TEST_SSH_PORT")?.parse().ok()?,
            user: exports.get("LAZYSYNC_TEST_SSH_USER")?.clone(),
            home: PathBuf::from(exports.get("LAZYSYNC_TEST_HOME")?),
            served: PathBuf::from(exports.get("LAZYSYNC_TEST_SERVED")?),
            work,
        })
    }

    /// A target pointing at the served directory, authenticating with the
    /// fixture's key.
    fn target(&self) -> Target {
        let mut target = Target::new(Protocol::Sftp, "127.0.0.1");
        target.port = Some(self.port);
        target.username = Some(self.user.clone());
        target.auth = AuthMethod::Key;
        target.identity_file = Some(self.home.join(".ssh/id_ed25519"));
        target.remote_path = Some(self.served.clone());
        target
    }

    fn known_hosts(&self) -> PathBuf {
        self.home.join(".ssh/known_hosts")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sshd.sh");
        let _ = Command::new("bash")
            .arg(script)
            .arg("stop")
            .arg(&self.work)
            .status();
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// Run `body` with `HOME` pointed at the fixture.
///
/// Serialised through a mutex because `set_var` is process-wide: two tests
/// changing `HOME` at once would read each other's `known_hosts`.
fn with_home<T>(home: &Path, body: impl FnOnce() -> T) -> T {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());

    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = std::env::var_os("HOME");

    // SAFETY: the mutex above makes this the only thread touching HOME, and
    // the value is restored before the guard drops.
    unsafe { std::env::set_var("HOME", home) };
    let result = body();
    match previous {
        Some(value) => unsafe { std::env::set_var("HOME", value) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    result
}

macro_rules! server {
    () => {
        match Server::start() {
            Some(server) => server,
            None => return,
        }
    };
}

/// The security property the whole pre-flight exists for: a server lazysync
/// has never seen must come back as unknown, not as trusted.
#[test]
fn a_first_connection_reports_an_unknown_host_key() {
    let server = server!();

    with_home(&server.home, || {
        // The fixture's known_hosts does not list the server yet.
        let _ = std::fs::remove_file(server.known_hosts());

        let verdict = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        );

        match verdict {
            Ok((Verdict::Unknown { fingerprint }, key)) => {
                assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
                assert_eq!(key.fingerprint(), fingerprint);
                assert_eq!(key.host_port(), format!("127.0.0.1:{}", server.port));
            }
            Ok((other, _)) => panic!("expected Unknown, got {other:?}"),
            Err(e) => panic!("verification failed: {e}"),
        }
    });
}

/// And the fingerprint must match what `ssh-keygen` reports for the same key,
/// since the only use of showing one is comparing it against that.
#[test]
fn the_fingerprint_matches_ssh_keygen() {
    let server = server!();

    let host_key_pub = server.work.join("host_ed25519.pub");
    let output = Command::new("ssh-keygen")
        .arg("-lf")
        .arg(&host_key_pub)
        .output()
        .expect("ssh-keygen -lf");
    let reported = String::from_utf8_lossy(&output.stdout);
    let expected = reported
        .split_whitespace()
        .find(|field| field.starts_with("SHA256:"))
        .expect("ssh-keygen should print a SHA256 fingerprint");

    with_home(&server.home, || {
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");

        assert_eq!(
            key.fingerprint(),
            expected,
            "a fingerprint that does not match ssh-keygen is worse than none: it trains you \
             to ignore the prompt"
        );
    });
}

/// Trusting a key records it, and the next check says so.
#[test]
fn trusting_a_key_records_it_and_it_verifies_afterwards() {
    let server = server!();

    with_home(&server.home, || {
        let _ = std::fs::remove_file(server.known_hosts());

        let (verdict, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("first verification");
        assert!(matches!(verdict, Verdict::Unknown { .. }));

        let written =
            hostkey::remember(&key, Some(&server.known_hosts())).expect("recording the host key");
        assert_eq!(written, server.known_hosts());

        let (verdict, _) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("second verification");
        assert_eq!(
            verdict,
            Verdict::Trusted,
            "the key just recorded should verify"
        );
    });
}

/// The recorded line has to be one OpenSSH itself accepts, which is why
/// libssh2's writer is used rather than hand-assembling the format.
#[test]
fn a_recorded_key_is_readable_by_ssh_keygen() {
    let server = server!();

    with_home(&server.home, || {
        let _ = std::fs::remove_file(server.known_hosts());
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        let output = Command::new("ssh-keygen")
            .arg("-F")
            .arg(format!("[127.0.0.1]:{}", server.port))
            .arg("-f")
            .arg(server.known_hosts())
            .output()
            .expect("ssh-keygen -F");

        assert!(
            output.status.success(),
            "ssh-keygen could not find the host in the known_hosts lazysync wrote: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    });
}

/// A changed key must be reported as changed, not as unknown. This is the
/// difference between a routine prompt and a refusal.
#[test]
fn a_changed_host_key_is_detected() {
    let server = server!();

    with_home(&server.home, || {
        // Record a *different* server's key against this host and port.
        let other = server.work.join("impostor");
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&other)
            .status()
            .expect("ssh-keygen");

        let pubkey = std::fs::read_to_string(other.with_extension("pub")).expect("read pub");
        let body = pubkey
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        std::fs::write(
            server.known_hosts(),
            format!("[127.0.0.1]:{} {body}\n", server.port),
        )
        .expect("write known_hosts");

        let (verdict, _) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");

        match verdict {
            Verdict::Changed { known_hosts, .. } => {
                assert_eq!(known_hosts, server.known_hosts());
            }
            other => panic!(
                "a mismatched key must be Changed, not {other:?} -- collapsing the two would \
                 turn an attack into a routine prompt"
            ),
        }
    });
}

/// The end-to-end case: connect and list a real remote directory.
#[test]
fn an_sftp_session_lists_a_remote_directory() {
    let server = server!();

    with_home(&server.home, || {
        let target = server.target();

        // Trust the key first; `open` assumes the precheck has passed.
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        let mut fs = ssh::open(&target, None).expect("open an SFTP session");

        assert_eq!(fs.scheme(), "sftp");
        assert!(fs.is_connected());

        let mut entries = fs.list(&server.served).expect("list the served directory");
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, [".hidden", "README.md", "docs", "run.sh", "src"]);

        // Metadata the confirmation window will depend on.
        let readme = entries.iter().find(|e| e.name == "README.md").unwrap();
        assert_eq!(readme.kind, EntryKind::File);
        assert_eq!(readme.size, 10, "the fixture writes exactly 'hello sftp'");
        assert!(
            readme.mtime.is_some(),
            "the sync planner needs a timestamp from the remote side"
        );
        assert!(readme.mode.is_some(), "SFTP reports POSIX mode bits");

        let src = entries.iter().find(|e| e.name == "src").unwrap();
        assert_eq!(src.kind, EntryKind::Dir);
        assert!(src.is_dir_like());
        assert_eq!(src.size, 0, "a directory's apparent size is meaningless");

        let run = entries.iter().find(|e| e.name == "run.sh").unwrap();
        assert!(
            run.is_executable(),
            "the fixture chmod 755s this, and the browser colours on it"
        );
    });
}

/// Capabilities have to describe the server honestly, because the planner's
/// comparison rules key off them.
#[test]
fn an_sftp_session_reports_honest_capabilities() {
    let server = server!();

    with_home(&server.home, || {
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        let fs = ssh::open(&server.target(), None).expect("open");
        let capabilities = fs.capabilities();

        assert!(capabilities.mtime);
        assert!(capabilities.mode, "SFTP carries POSIX permissions");
        assert!(capabilities.symlinks);
        assert!(
            capabilities.mtime_granularity_secs >= 1,
            "claiming sub-second precision the protocol may not carry is what makes a sync \
             re-copy an unchanged tree"
        );
    });
}

#[test]
fn navigating_into_a_subdirectory_works() {
    let server = server!();

    with_home(&server.home, || {
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        let mut fs = ssh::open(&server.target(), None).expect("open");

        let nested = server.served.join("src/api");
        assert!(fs.is_dir(&nested));

        let entries = fs.list(&nested).expect("list src/api");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "handler.rs");

        // And a path that is not there must read as absent rather than as an
        // error, which is what lets synchronized browsing ghost.
        assert!(!fs.is_dir(&server.served.join("nope")));
        let err = fs.list(&server.served.join("nope")).unwrap_err();
        assert!(
            err.is_not_found(),
            "synchronized browsing depends on this being not-found: {err}"
        );
    });
}

#[test]
fn creating_a_remote_directory_works() {
    let server = server!();

    with_home(&server.home, || {
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        let mut fs = ssh::open(&server.target(), None).expect("open");

        // Nested, to exercise the create-missing-parents walk that
        // `on_missing = "create"` relies on.
        let deep = server.served.join("made/by/lazysync");
        fs.create_dir(&deep).expect("create nested directories");
        assert!(deep.is_dir(), "the directories should exist on disk");

        // Doing it again is not an error: two panes following each other can
        // both ask for the same path.
        fs.create_dir(&deep).expect("creating twice should succeed");
    });
}

/// Wrong credentials have to surface as an authentication failure, because
/// that is the one error the user can act on by retyping something.
#[test]
fn a_bad_key_reports_an_authentication_failure() {
    let server = server!();

    with_home(&server.home, || {
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        // A valid key the server does not accept.
        let wrong = server.work.join("wrong_key");
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&wrong)
            .status()
            .expect("ssh-keygen");

        let mut target = server.target();
        target.identity_file = Some(wrong);

        match ssh::open(&target, None) {
            Err(VfsError::AuthFailed(_)) => {}
            Err(other) => panic!("expected AuthFailed, got {other}"),
            Ok(_) => panic!("a key the server does not know must not authenticate"),
        }
    });
}

/// Planning and applying a sync over a real SSH connection.
///
/// The local-to-local tests cover the planner's logic; this covers the part
/// only a network backend exercises -- writing through SFTP, the partial-file
/// rename on the remote side, and whether a timestamp survives the trip. That
/// last one decides whether the *next* sync re-copies everything, and no mock
/// would tell the truth about it.
#[test]
fn a_push_over_sftp_transfers_and_preserves_timestamps() {
    use lazysync::{
        config::Config,
        sync::{
            diff::{self, Direction},
            executor,
            safety::DeletePolicy,
            transfer::Settings,
        },
        vfs::LocalFs,
    };

    let server = server!();

    with_home(&server.home, || {
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        let mut remote = ssh::open(&server.target(), None).expect("open");

        // A local tree to push: one new file and one that differs.
        let local_dir = server.work.join("local");
        std::fs::create_dir_all(local_dir.join("nested")).unwrap();
        std::fs::write(local_dir.join("fresh.txt"), b"brand new").unwrap();
        std::fs::write(local_dir.join("nested/deep.txt"), b"deeper").unwrap();
        // Same name as a file the server already has, different content.
        std::fs::write(local_dir.join("README.md"), b"replaced from local").unwrap();

        let mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
        for name in ["fresh.txt", "nested/deep.txt", "README.md"] {
            filetime::set_file_mtime(local_dir.join(name), mtime).unwrap();
        }

        let mut local = LocalFs::new();
        let config = Config::default();

        let plan = diff::plan(
            diff::Endpoint {
                fs: &mut local,
                root: &local_dir,
                label: "local".into(),
            },
            diff::Endpoint {
                fs: &mut remote,
                root: &server.served,
                label: "remote".into(),
            },
            Direction {
                source: 0,
                delete_extraneous: false,
            },
            &config,
        );

        // The plan must name both sides for the file that exists on each.
        let readme = plan
            .rows
            .iter()
            .find(|r| r.path == Path::new("README.md"))
            .expect("README.md should be in the plan");
        assert!(
            readme.source.is_some() && readme.dest.is_some(),
            "the window needs both sides of a remote overwrite"
        );
        assert!(
            readme.dest.unwrap().mtime.is_some(),
            "the remote side has to contribute a timestamp"
        );

        let report = executor::apply(
            &mut local,
            &local_dir,
            &mut remote,
            &server.served,
            &plan,
            &Settings::transient(DeletePolicy::Remove),
        );

        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

        // The server writes to a real directory, so the result is checkable
        // directly rather than through the client that wrote it.
        assert_eq!(
            std::fs::read_to_string(server.served.join("fresh.txt")).unwrap(),
            "brand new"
        );
        assert_eq!(
            std::fs::read_to_string(server.served.join("nested/deep.txt")).unwrap(),
            "deeper",
            "a nested create has to bring its directory with it, over SFTP too"
        );
        assert_eq!(
            std::fs::read_to_string(server.served.join("README.md")).unwrap(),
            "replaced from local"
        );

        // No partial files left in the remote tree: one would show up in the
        // next plan as a file to sync back.
        let leftovers: Vec<_> = std::fs::read_dir(&server.served)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("lazysync-part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // The timestamp survived. If it did not, the next sync would see every
        // file as changed and copy the whole tree again.
        let landed = filetime::FileTime::from_last_modification_time(
            &std::fs::metadata(server.served.join("fresh.txt")).unwrap(),
        );
        assert_eq!(
            landed.unix_seconds(),
            1_700_000_000,
            "an mtime lost in transit makes every subsequent sync re-copy the file"
        );
    });
}

/// And the proof of that: planning again finds nothing to do.
#[test]
fn a_second_push_over_sftp_finds_nothing_to_do() {
    use lazysync::{
        config::Config,
        sync::{
            diff::{self, Direction},
            executor,
            safety::DeletePolicy,
            transfer::Settings,
        },
        vfs::LocalFs,
    };

    let server = server!();

    with_home(&server.home, || {
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        let mut remote = ssh::open(&server.target(), None).expect("open");

        let local_dir = server.work.join("local2");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::write(local_dir.join("a.txt"), b"content").unwrap();
        filetime::set_file_mtime(
            local_dir.join("a.txt"),
            filetime::FileTime::from_unix_time(1_700_000_000, 0),
        )
        .unwrap();

        let mut local = LocalFs::new();
        let config = Config::default();

        let build = |local: &mut LocalFs, remote: &mut lazysync::vfs::RemoteAdapter| {
            diff::plan(
                diff::Endpoint {
                    fs: local,
                    root: &local_dir,
                    label: "local".into(),
                },
                diff::Endpoint {
                    fs: remote,
                    root: &server.served,
                    label: "remote".into(),
                },
                Direction {
                    source: 0,
                    delete_extraneous: false,
                },
                &config,
            )
        };

        let first = build(&mut local, &mut remote);
        assert_eq!(first.totals().operations(), 1);

        let report = executor::apply(
            &mut local,
            &local_dir,
            &mut remote,
            &server.served,
            &first,
            &Settings::transient(DeletePolicy::Remove),
        );
        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

        let second = build(&mut local, &mut remote);
        assert_eq!(
            second.totals().operations(),
            0,
            "a sync that re-copies an unchanged tree is the failure this whole comparison \
             design exists to avoid: {:?}",
            second
                .rows
                .iter()
                .filter(|r| r.needs_review())
                .map(|r| (&r.path, r.reason))
                .collect::<Vec<_>>()
        );
    });
}

/// Connecting to a port nothing is listening on must fail promptly rather than
/// hanging, because the blocking `Vfs` means a hang blocks the draw loop.
#[test]
fn an_unreachable_host_fails_rather_than_hanging() {
    let mut target = Target::new(Protocol::Sftp, "127.0.0.1");
    // Port 1 on loopback: refused immediately rather than timing out.
    target.port = Some(1);

    let started = std::time::Instant::now();
    let result = ssh::precheck(&target);

    assert!(result.is_err(), "nothing is listening on port 1");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(16),
        "took {:?}, which is past the connect timeout",
        started.elapsed()
    );
}

/// Bisync over a real SSH connection, including a deletion propagating.
///
/// The local-to-local tests cover the classification matrix; this covers what
/// only a network backend exercises. Two things in particular: the per-row
/// direction has to survive a plan whose two sides are different *kinds* of
/// backend, and the snapshot has to record what actually landed on the remote
/// rather than what was sent -- which is where a backend that rounds or drops
/// a timestamp would make the next run see a phantom edit.
#[test]
fn a_bisync_over_sftp_moves_both_ways_and_then_settles() {
    use lazysync::{
        config::Config,
        sync::{
            bisync, diff, executor,
            safety::DeletePolicy,
            state::{Record, Snapshot},
            transfer::Settings,
        },
        vfs::LocalFs,
    };

    let server = server!();

    with_home(&server.home, || {
        let (_, key) = hostkey::verify(
            "127.0.0.1",
            server.port,
            std::time::Duration::from_secs(10),
            Some(&server.known_hosts()),
        )
        .expect("verification");
        hostkey::remember(&key, Some(&server.known_hosts())).expect("recording");

        let mut remote = ssh::open(&server.target(), None).expect("open");
        let mut local = LocalFs::new();
        let config = Config::default();

        let local_dir = server.work.join("bisync-local");
        std::fs::create_dir_all(&local_dir).unwrap();
        std::fs::write(local_dir.join("from-local.txt"), b"L").unwrap();
        std::fs::write(server.served.join("from-remote.txt"), b"R").unwrap();

        // The snapshot is built in memory rather than read from disk: this
        // test is about the planner and executor over a real connection, not
        // about where the file lives.
        let mut snapshot = Snapshot::new(&local_dir, "sftp://127.0.0.1/served");

        let build = |local: &mut LocalFs,
                     remote: &mut lazysync::vfs::RemoteAdapter,
                     snapshot: &Snapshot| {
            bisync::plan(
                diff::Endpoint {
                    fs: local,
                    root: &local_dir,
                    label: "local".into(),
                },
                diff::Endpoint {
                    fs: remote,
                    root: &server.served,
                    label: "remote".into(),
                },
                snapshot,
                &config,
                &Default::default(),
                &mut lazysync::sync::progress::Silent,
            )
        };

        // ── First run: each side's file crosses to the other ──────────────
        let plan = build(&mut local, &mut remote, &snapshot);
        assert!(plan.bidirectional);
        assert!(plan.first_run);
        assert_eq!(
            plan.totals().deletes,
            0,
            "a first bisync has no basis for calling anything a deletion"
        );

        let report = executor::apply(
            &mut local,
            &local_dir,
            &mut remote,
            &server.served,
            &plan,
            &Settings::transient(DeletePolicy::Remove),
        );
        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

        // Checked on the server's own disk, not through the client that wrote
        // it: a backend that misreported what it did would otherwise agree
        // with itself.
        assert_eq!(
            std::fs::read_to_string(server.served.join("from-local.txt")).unwrap(),
            "L",
            "the local file should have reached the remote"
        );
        assert_eq!(
            std::fs::read_to_string(local_dir.join("from-remote.txt")).unwrap(),
            "R",
            "and the remote file the local side -- the direction a one-way plan cannot express"
        );

        // Record the pair's state as the app does, from what is now on disk.
        for name in ["from-local.txt", "from-remote.txt"] {
            let here = local.stat(&local_dir.join(name)).expect("local stat");
            let there = remote.stat(&server.served.join(name)).expect("remote stat");
            snapshot.insert(
                std::path::Path::new(name),
                Record::from_entries(&here, &there),
            );
        }
        snapshot.taken = Some(chrono::Utc::now());

        // ── Second run: nothing to do ─────────────────────────────────────
        let settled = build(&mut local, &mut remote, &snapshot);
        assert_eq!(
            settled.totals().operations(),
            0,
            "a settled pair must not re-copy: {:?}",
            settled
                .rows
                .iter()
                .filter(|r| r.needs_review())
                .map(|r| (&r.path, r.action, r.reason))
                .collect::<Vec<_>>()
        );

        // ── Third run: a deletion propagates to the remote ────────────────
        std::fs::remove_file(local_dir.join("from-remote.txt")).unwrap();

        let plan = build(&mut local, &mut remote, &snapshot);
        assert_eq!(
            plan.totals().deletes,
            1,
            "with history, a removal on one side should propagate: {:?}",
            plan.rows
                .iter()
                .filter(|r| r.needs_review())
                .map(|r| (&r.path, r.action, r.reason))
                .collect::<Vec<_>>()
        );

        let report = executor::apply(
            &mut local,
            &local_dir,
            &mut remote,
            &server.served,
            &plan,
            &Settings::transient(DeletePolicy::Remove),
        );
        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

        assert!(
            !server.served.join("from-remote.txt").exists(),
            "the deletion should have reached the remote"
        );
        assert!(
            server.served.join("from-local.txt").exists(),
            "and the untouched file should still be there"
        );
    });
}
