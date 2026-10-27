//! FTP against a real server.
//!
//! A protocol backend tested only against a mock agrees with the mock and
//! nothing else, so these run against an actual FTP server started by
//! `tests/fixtures/ftpd.sh` -- loopback only, on a high port, serving a
//! throwaway directory.
//!
//! FTP earns this more than most protocols. Its directory listing format is
//! not standardised, so parsing one is guesswork until a server has produced
//! it. Its working directory is stateful, so every path we send depends on
//! where the session happens to be. Data moves over a *second* connection
//! negotiated by `PASV`, which a mock does not model at all. And `MFMT` --
//! setting a modification time -- is an extension many servers lack, which
//! turns out to decide whether a sync re-copies its whole tree on every run.
//! See [`a_second_push_over_ftp_finds_nothing_to_do`], which is the test that
//! caught it.
//!
//! They skip when the fixture cannot run, rather than failing: a contributor
//! without `pyftpdlib` should still get a green `cargo test`. The Nix dev
//! shell provides it, so the skip does not quietly hide a regression for
//! anyone following CONTRIBUTING.md.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use lazysync::{
    connection::{Protocol, Secret, Target},
    vfs::{ftp, EntryKind, Vfs, VfsError},
};

/// A running fixture, torn down on drop.
struct Server {
    work: PathBuf,
    port: u16,
    /// A second server on which TLS is required, with a self-signed
    /// certificate. `None` when the fixture could not produce one.
    tls_port: Option<u16>,
    user: String,
    pass: String,
    /// The directory the server exposes, on this machine's disk.
    ///
    /// Results are checked here rather than through the client that wrote
    /// them: a backend that misreports what it did would otherwise agree with
    /// itself.
    root: PathBuf,
}

impl Server {
    /// Start one, or `None` if the fixture cannot run here.
    fn start() -> Option<Self> {
        if which("python3").is_none() {
            eprintln!("skipping: no python3");
            return None;
        }

        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ftpd.sh");

        // A directory per *server*, not per process: cargo runs these in
        // parallel within one binary and the fixture script starts by
        // `rm -rf`ing its work directory, so a shared path means one test
        // deletes another's served tree out from under it.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let work = std::env::temp_dir().join(format!(
            "lazysync-ftpd-{}-{}",
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
                "skipping: the ftpd fixture failed: {}",
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
            port: exports.get("LAZYSYNC_TEST_FTP_PORT")?.parse().ok()?,
            // Absent when openssl could not make a certificate, which is a
            // skip rather than a failure.
            tls_port: exports
                .get("LAZYSYNC_TEST_FTPS_PORT")
                .and_then(|value| value.parse().ok()),
            user: exports.get("LAZYSYNC_TEST_FTP_USER")?.clone(),
            pass: exports.get("LAZYSYNC_TEST_FTP_PASS")?.clone(),
            root: PathBuf::from(exports.get("LAZYSYNC_TEST_FTP_ROOT")?),
            work,
        })
    }

    fn target(&self) -> Target {
        let mut target = Target::new(Protocol::Ftp, "127.0.0.1");
        target.port = Some(self.port);
        target.username = Some(self.user.clone());
        target
    }

    fn secret(&self) -> Secret {
        Secret::new(self.pass.clone())
    }

    fn open(&self) -> lazysync::vfs::RemoteAdapter {
        ftp::open(&self.target(), Some(&self.secret())).expect("open an FTP session")
    }

    /// The server confines the login to its served directory, so every path
    /// the client sends is relative to that -- `/` *is* `self.root`.
    fn remote(&self, relative: &str) -> PathBuf {
        if relative.is_empty() {
            PathBuf::from("/")
        } else {
            Path::new("/").join(relative)
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ftpd.sh");
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

macro_rules! server {
    () => {
        match Server::start() {
            Some(server) => server,
            None => return,
        }
    };
}

/// The end-to-end case: connect and list a real remote directory.
///
/// The sizes are asserted exactly, because the point of running a real server
/// is that its LIST output is parsed rather than imagined -- a parser that
/// grabbed the wrong column would still produce plausible-looking entries.
#[test]
fn an_ftp_session_lists_a_remote_directory() {
    let server = server!();
    let mut fs = server.open();

    assert_eq!(fs.scheme(), "ftp");
    assert!(fs.is_connected());

    let mut entries = fs
        .list(&server.remote(""))
        .expect("list the served directory");
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, [".hidden", "README.md", "docs", "src"]);

    let readme = entries.iter().find(|e| e.name == "README.md").unwrap();
    assert_eq!(readme.kind, EntryKind::File);
    assert_eq!(
        readme.size, 9,
        "the fixture writes exactly 'hello ftp'; a misread LIST column would still look \
         like a number"
    );
    assert!(
        readme.mtime.is_some(),
        "FTP does report a modification time -- it is setting one it cannot do"
    );

    let hidden = entries.iter().find(|e| e.name == ".hidden").unwrap();
    assert_eq!(hidden.size, 1);
    assert!(hidden.is_hidden());

    let src = entries.iter().find(|e| e.name == "src").unwrap();
    assert_eq!(
        src.kind,
        EntryKind::Dir,
        "a LIST line's leading `d` is the only thing distinguishing a directory here"
    );
    assert!(src.is_dir_like());
}

/// Capabilities have to describe the protocol honestly, because the planner's
/// comparison rules key off them.
#[test]
fn an_ftp_session_reports_honest_capabilities() {
    let server = server!();
    let fs = server.open();
    let capabilities = fs.capabilities();

    assert!(capabilities.mtime, "MDTM and LIST both carry one");
    assert!(
        !capabilities.set_mtime,
        "MFMT is not in the base protocol; claiming it would make the planner compare \
         timestamps that can never agree"
    );
    assert!(!capabilities.mode, "FTP has no Unix mode bits of its own");
    assert!(
        capabilities.mtime_granularity_secs >= 1,
        "LIST reports minutes and MDTM seconds; sub-second precision is not on offer"
    );
}

#[test]
fn navigating_into_a_subdirectory_works() {
    let server = server!();
    let mut fs = server.open();

    let nested = server.remote("src/api");
    assert!(fs.is_dir(&nested));

    let entries = fs.list(&nested).expect("list src/api");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "handler.rs");
    assert_eq!(entries[0].size, 6);

    // And a path that is not there must read as absent rather than as an
    // error, which is what lets synchronized browsing ghost a position
    // instead of showing a failure.
    assert!(!fs.is_dir(&server.remote("nope")));
    let err = fs.list(&server.remote("nope")).unwrap_err();
    assert!(
        err.is_not_found(),
        "synchronized browsing depends on this being not-found: {err}"
    );
}

#[test]
fn creating_a_remote_directory_works() {
    let server = server!();
    let mut fs = server.open();

    // Nested, to exercise the create-missing-parents walk that
    // `sync_browse.on_missing = "create"` relies on. FTP has no mkdir -p:
    // `MKD` makes exactly one level, so the walk is ours.
    fs.create_dir(&server.remote("made/by/lazysync"))
        .expect("create nested directories");
    assert!(
        server.root.join("made/by/lazysync").is_dir(),
        "the directories should exist on the server's disk"
    );

    // Doing it again is not an error: two panes following each other can both
    // ask for the same path.
    fs.create_dir(&server.remote("made/by/lazysync"))
        .expect("creating twice should succeed");
}

/// Wrong credentials have to surface as an authentication failure, because
/// that is the one error the user can act on by retyping something -- and a
/// bookmark prompts for exactly that.
#[test]
fn a_bad_password_reports_an_authentication_failure() {
    let server = server!();

    match ftp::open(&server.target(), Some(&Secret::new("not-the-password"))) {
        Err(VfsError::AuthFailed(_)) => {}
        Err(other) => panic!("expected AuthFailed, got {other}"),
        Ok(_) => panic!("a password the server does not accept must not authenticate"),
    }
}

/// Logging in lands in the account's own directory, which is what makes
/// `ftp://host` with no path open somewhere useful.
#[test]
fn the_session_reports_a_home_directory() {
    let server = server!();
    let mut fs = server.open();

    let home = fs.home().expect("PWD after login should give a home");
    assert!(
        home.is_absolute(),
        "a relative home would be interpreted against whatever the session's cwd later \
         became: {home:?}"
    );
    assert!(fs.is_dir(&home), "the home it reports has to be listable");
}

/// Planning and applying a sync over a real FTP connection.
///
/// The local-to-local tests cover the planner's logic; this covers what only a
/// network backend exercises -- writing over a `PASV` data connection, the
/// partial-file rename on the remote side, and the directory walk.
#[test]
fn a_push_over_ftp_transfers_files() {
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
    let mut remote = server.open();

    // A local tree to push: two new files and one that differs.
    let local_dir = server.work.join("local");
    std::fs::create_dir_all(local_dir.join("nested")).unwrap();
    std::fs::write(local_dir.join("fresh.txt"), b"brand new").unwrap();
    std::fs::write(local_dir.join("nested/deep.txt"), b"deeper").unwrap();
    // Same name as a file the server already has, different content.
    std::fs::write(local_dir.join("README.md"), b"replaced from local").unwrap();

    let mut local = LocalFs::new();
    let config = Config::default();
    let remote_root = server.remote("");

    let plan = diff::plan(
        diff::Endpoint {
            fs: &mut local,
            root: &local_dir,
            label: "local".into(),
        },
        diff::Endpoint {
            fs: &mut remote,
            root: &remote_root,
            label: "remote".into(),
        },
        Direction {
            source: 0,
            delete_extraneous: false,
        },
        &config,
    );

    // The confirmation window's whole purpose: both sides of an overwrite.
    let readme = plan
        .rows
        .iter()
        .find(|r| r.path == Path::new("README.md"))
        .expect("README.md should be in the plan");
    assert!(
        readme.source.is_some() && readme.dest.is_some(),
        "the window needs both sides of a remote overwrite"
    );
    assert_eq!(
        readme.dest.unwrap().size,
        9,
        "the size being overwritten is read off the server, not guessed"
    );

    let report = executor::apply(
        &mut local,
        &local_dir,
        &mut remote,
        &remote_root,
        &plan,
        &Settings::transient(DeletePolicy::Remove),
    );

    assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

    assert_eq!(
        std::fs::read_to_string(server.root.join("fresh.txt")).unwrap(),
        "brand new"
    );
    assert_eq!(
        std::fs::read_to_string(server.root.join("nested/deep.txt")).unwrap(),
        "deeper",
        "a nested create has to bring its directory with it, over FTP too"
    );
    assert_eq!(
        std::fs::read_to_string(server.root.join("README.md")).unwrap(),
        "replaced from local",
        "the overwrite path is the one FTP makes awkward: no atomic replacing rename"
    );

    // No partial files left in the remote tree: one would show up in the next
    // plan as a file to sync back.
    let leftovers: Vec<_> = std::fs::read_dir(&server.root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("lazysync-part"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// The test that mattered: a second sync of an unchanged tree must find
/// nothing to do.
///
/// FTP cannot set a modification time, so every file it receives carries the
/// time it was *uploaded*. Comparing that against the source's timestamp can
/// never agree, which meant the second run re-copied the entire tree -- for
/// ever, on every run. No mock would have shown this: a mock that stores
/// whatever mtime it is handed makes the comparison pass.
///
/// The fix is in `compare`: a timestamp comparison needs the destination to be
/// able to *preserve* the source's time, not merely to report one, so FTP
/// degrades to a size comparison and the window says `size` as the method it
/// used.
#[test]
fn a_second_push_over_ftp_finds_nothing_to_do() {
    use lazysync::{
        config::Config,
        sync::{
            compare::{Method, Reason},
            diff::{self, Direction},
            executor,
            safety::DeletePolicy,
            transfer::Settings,
        },
        vfs::LocalFs,
    };

    let server = server!();
    let mut remote = server.open();

    let local_dir = server.work.join("local2");
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::write(local_dir.join("a.txt"), b"content").unwrap();
    // Deliberately old, so an mtime comparison against the upload time cannot
    // accidentally fall inside the tolerance window.
    filetime::set_file_mtime(
        local_dir.join("a.txt"),
        filetime::FileTime::from_unix_time(1_700_000_000, 0),
    )
    .unwrap();

    let mut local = LocalFs::new();
    let config = Config::default();
    let remote_root = server.remote("");

    let build = |local: &mut LocalFs, remote: &mut lazysync::vfs::RemoteAdapter| {
        diff::plan(
            diff::Endpoint {
                fs: local,
                root: &local_dir,
                label: "local".into(),
            },
            diff::Endpoint {
                fs: remote,
                root: &remote_root,
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
        &remote_root,
        &first,
        &Settings::transient(DeletePolicy::Remove),
    );
    assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

    let second = build(&mut local, &mut remote);
    assert_eq!(
        second.totals().operations(),
        0,
        "a sync that re-copies an unchanged tree is the failure the whole comparison design \
         exists to avoid: {:?}",
        second
            .rows
            .iter()
            .filter(|r| r.needs_review())
            .map(|r| (&r.path, r.reason))
            .collect::<Vec<_>>()
    );

    // And it has to say *why* it considered them equal. "same size and time"
    // would be a lie here -- the times differ by years and were not consulted.
    let row = second
        .rows
        .iter()
        .find(|r| r.path == Path::new("a.txt"))
        .expect("a.txt should still be in the plan as a skip");
    assert_eq!(row.method, Method::SizeOnly);
    assert_eq!(
        row.reason,
        Reason::SameSizeTimesNotPreserved,
        "the window must not claim a timestamp check it could not make"
    );
}

/// Reading back over FTP, which is the pull direction and a separate code path
/// from the write.
#[test]
fn a_remote_file_can_be_read_back() {
    let server = server!();
    let mut fs = server.open();

    let mut buffer = Vec::new();
    let bytes = fs
        .read_into(&server.remote("README.md"), &mut buffer)
        .expect("read a remote file");

    assert_eq!(bytes, 9);
    assert_eq!(String::from_utf8_lossy(&buffer), "hello ftp");
}

/// The declared mtime granularity has to cover what `LIST` actually loses.
///
/// `remotefs-ftp` lists with `LIST`, whose format is `ls -l` output: a recent
/// file shows `Oct 24 22:52` and loses the seconds, while one older than about
/// six months shows `Nov 14  2023` and loses the time entirely. So the error
/// is not a second or a minute -- it is up to a full day.
///
/// Why it matters: on a *pull* the FTP side is the source and the local side
/// the destination, so timestamps are compared for real. Declaring 1 second
/// would put every remote timestamp outside tolerance and make an identical
/// tree read as completely changed.
#[test]
fn the_declared_mtime_granularity_covers_what_list_loses() {
    let server = server!();

    // Two files: one from today, one from 2023, both with non-zero seconds so
    // the rounding is visible.
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let cases = [
        ("recent.txt", today / 60 * 60 + 17),
        ("old.txt", 1_700_000_017),
    ];

    for (name, epoch) in cases {
        std::fs::write(server.root.join(name), b"xxxxx").unwrap();
        filetime::set_file_mtime(
            server.root.join(name),
            filetime::FileTime::from_unix_time(epoch, 0),
        )
        .unwrap();
    }

    let mut fs = server.open();
    let entries = fs.list(&server.remote("")).expect("list");
    let granularity = Protocol::Ftp.capabilities().mtime_granularity_secs as i64;

    let mut worst = 0;
    for (name, epoch) in cases {
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} should be listed"));
        let reported = entry.mtime.expect("FTP reports a time").timestamp();
        let error = (reported - epoch).abs();
        worst = worst.max(error);

        assert!(
            error <= granularity,
            "{name}: the server reported a time {error}s from the real one, outside the \
             {granularity}s granularity lazysync claims. A claim narrower than the protocol's \
             real precision makes an unchanged tree look entirely changed."
        );
    }

    // And the claim must not be wildly loose either -- that costs real
    // detection. One day is the honest figure; a week would not be.
    assert!(
        granularity <= 86_400,
        "granularity of {granularity}s is looser than LIST's actual worst case"
    );
    assert!(
        worst > 60,
        "the old-file case should have lost more than a minute ({worst}s); if LIST started \
         carrying seconds, this claim could be tightened"
    );
}

/// An FTPS server whose certificate is signed by nobody must be **refused**.
///
/// There is deliberately no accept-invalid-certs option, so this is the whole
/// of what `ftps://` promises: a session that silently trusted any certificate
/// would be worse than a plaintext one, because the scheme in the status bar
/// would say it was safe.
///
/// A self-signed certificate is the only kind a test can mint, so this fixture
/// can only prove the refusal, never a successful handshake. The refusal is
/// the half worth pinning.
#[test]
fn an_untrusted_ftps_certificate_is_refused() {
    let server = server!();
    let Some(tls_port) = server.tls_port else {
        eprintln!("skipping: no openssl, so no certificate to be untrusted");
        return;
    };

    let mut target = server.target();
    target.protocol = Protocol::Ftps;
    target.port = Some(tls_port);

    match ftp::open(&target, Some(&server.secret())) {
        Ok(_) => panic!(
            "a certificate signed by nobody was accepted -- `ftps://` would then be a claim \
             lazysync cannot back"
        ),
        Err(VfsError::AuthFailed(message)) => panic!(
            "refused, but reported as an authentication failure: {message}. That invites \
             retyping the password at a server whose certificate did not verify"
        ),
        Err(other) => {
            // The *reason* has to survive into the message, not just the
            // failure. "could not connect" would send someone to check their
            // firewall over what is actually an untrusted certificate -- and
            // it would let this test keep passing if the TLS layer were
            // removed entirely, which is how it passed once already.
            let message = other.to_string().to_lowercase();
            assert!(
                [
                    "certificate",
                    "tls",
                    "ssl",
                    "secure",
                    "verif",
                    "unknown issuer"
                ]
                .iter()
                .any(|needle| message.contains(needle)),
                "the error has to say it was a certificate problem, or the user goes looking \
                 at the wrong thing: {other}"
            );
        }
    }
}

/// And the plaintext port must stay plaintext. `ftps://` against a server that
/// does not offer `AUTH TLS` has to fail rather than quietly continuing in the
/// clear.
#[test]
fn ftps_against_a_plaintext_server_does_not_fall_back() {
    let server = server!();

    let mut target = server.target();
    target.protocol = Protocol::Ftps;
    // The plaintext server, which has no certificate at all.
    target.port = Some(server.port);

    assert!(
        ftp::open(&target, Some(&server.secret())).is_err(),
        "an `ftps://` connection that completes without TLS is the one outcome that must \
         never happen quietly"
    );
}

/// Connecting to a port nothing is listening on must fail promptly rather than
/// hanging, because the blocking `Vfs` means a hang blocks the draw loop.
#[test]
fn an_unreachable_host_fails_rather_than_hanging() {
    let mut target = Target::new(Protocol::Ftp, "127.0.0.1");
    // Port 1 on loopback: refused immediately rather than timing out.
    target.port = Some(1);

    let started = std::time::Instant::now();
    let result = ftp::open(&target, None);

    assert!(result.is_err(), "nothing is listening on port 1");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(16),
        "a refused connection took {:?}; the UI thread is blocked for that whole time",
        started.elapsed()
    );
}

/// Leaving the browser with a live connection must ask first.
///
/// `c` goes back to the connection list, and the list cannot navigate back
/// to a live browser -- so the only route back is to open a bookmark again,
/// which reconnects. That makes `c` a session-ending key sitting next to
/// nothing destructive, which is exactly the shape of a keystroke worth
/// confirming.
///
/// Driven against a real server rather than a stub, because the branch turns
/// on a pane holding something that is genuinely not the local filesystem.
#[test]
fn closing_a_live_connection_asks_first() {
    use lazysync::{
        app::{App, Overlay, Pending, Screen},
        browser::Pane,
        config::Config,
        keys::Action,
    };

    let server = server!();
    let config = Config::default();

    let here = server.work.clone();
    let mut app = App::new(config.clone(), here.clone(), here).expect("build an app");
    app.panes[1] = Pane::new(Box::new(server.open()), server.remote(""), &config);
    app.screen = Screen::Browser;

    assert_eq!(app.panes[1].vfs.scheme(), "ftp", "the fixture is connected");

    app.handle(Action::ShowConnections);

    match &app.overlay {
        Overlay::Question(question) => {
            assert!(
                question.text.contains("Close the connection"),
                "unexpected question: {:?}",
                question.text
            );
            assert!(matches!(question.on_yes, Pending::LeaveConnection));
        }
        other => panic!("expected a confirmation, got {other:?}"),
    }
    assert_eq!(
        app.screen,
        Screen::Browser,
        "asking must not already have left"
    );

    // Answering yes ends the session: the remote pane is replaced, not just
    // covered by another screen.
    app.handle(Action::Apply);
    assert_eq!(app.screen, Screen::Bookmarks);
    assert_eq!(
        app.panes[1].vfs.scheme(),
        "local",
        "the connection must actually be closed, since that is what was asked"
    );
}
