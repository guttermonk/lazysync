//! The rclone backend, against the real `rclone` binary.
//!
//! Tested with a remote of type `local` pointed at a temporary directory. That
//! is a real rclone remote going through rclone's full config, VFS and
//! command-line machinery -- the only thing it is not exercising is a network,
//! which is exactly the part lazysync is not responsible for. Everything that
//! could be wrong on lazysync's side (the `remote:path` spelling, lsjson
//! parsing, streaming through cat and rcat, how a missing path is classified)
//! is covered.
//!
//! The payoff being verified: this is the route to S3, WebDAV, B2 and Google
//! Drive without waiting for `remotefs 1.0` ports of any of them.

use std::{fs, path::Path};

use lazysync::vfs::{rclone::RcloneFs, EntryKind, Vfs, WriteMetadata};
use tempfile::TempDir;

/// A `local` remote in a throwaway rclone config.
struct Remote {
    _dir: TempDir,
    root: std::path::PathBuf,
    name: String,
    /// Held so the env var outlives the test body.
    _guard: std::sync::MutexGuard<'static, ()>,
}

static CONFIG: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Remote {
    fn start() -> Option<Self> {
        if !lazysync::sync::external::available("rclone") {
            eprintln!("skipping: rclone is not installed");
            return None;
        }

        let guard = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().ok()?;
        let root = dir.path().join("served");
        fs::create_dir_all(root.join("sub")).ok()?;

        // An `alias` remote, which maps `lstest:x` onto `<root>/x`. A `local`
        // remote would need the absolute path on every reference, and
        // lazysync strips the leading slash from an rclone path -- correctly,
        // since `gdrive:/Documents` and `gdrive:Documents` are different
        // things to an object store. An alias is what gives the remote a root
        // of its own, which is what every real remote has.
        let config = dir.path().join("rclone.conf");
        fs::write(
            &config,
            format!("[lstest]\ntype = alias\nremote = {}\n", root.display()),
        )
        .ok()?;

        // SAFETY: the mutex above makes this the only thread touching it.
        unsafe { std::env::set_var("RCLONE_CONFIG", &config) };

        Some(Self {
            name: "lstest".to_string(),
            root,
            _dir: dir,
            _guard: guard,
        })
    }

    /// A backend for this remote.
    fn fs(&self) -> RcloneFs {
        RcloneFs::open(&self.name).expect("rclone is available")
    }
}

macro_rules! remote {
    () => {
        match Remote::start() {
            Some(remote) => remote,
            None => return,
        }
    };
}

#[test]
fn a_listing_comes_back_with_sizes_and_times() {
    let remote = remote!();
    fs::write(remote.root.join("README.md"), b"hello rclone").unwrap();
    fs::write(remote.root.join("sub/nested.txt"), b"deep").unwrap();

    let mut fs_ = remote.fs();
    assert_eq!(fs_.scheme(), "rclone");

    let mut entries = fs_.list(Path::new("/")).expect("a listing");
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["README.md", "sub"]);

    let readme = entries.iter().find(|e| e.name == "README.md").unwrap();
    assert_eq!(readme.kind, EntryKind::File);
    assert_eq!(readme.size, 12, "lsjson's Size column, parsed");
    assert!(
        readme.mtime.is_some(),
        "the planner needs a timestamp from every side it compares"
    );

    let sub = entries.iter().find(|e| e.name == "sub").unwrap();
    assert_eq!(sub.kind, EntryKind::Dir);
    assert!(sub.is_dir_like());
}

#[test]
fn navigating_into_a_subdirectory_works() {
    let remote = remote!();
    fs::write(remote.root.join("sub/nested.txt"), b"deep").unwrap();

    let mut fs_ = remote.fs();
    assert!(fs_.is_dir(Path::new("/sub")));

    let entries = fs_.list(Path::new("/sub")).expect("a listing");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "nested.txt");
}

/// Synchronized browsing ghosts a missing counterpart rather than reporting a
/// failure, so this classification is what keeps the two panes coupled.
#[test]
fn a_missing_directory_reads_as_not_found() {
    let remote = remote!();
    let mut fs_ = remote.fs();

    assert!(!fs_.is_dir(Path::new("/nope")));

    let error = fs_.list(Path::new("/nope")).unwrap_err();
    assert!(
        error.is_not_found(),
        "ghosting depends on this being not-found, not a protocol error: {error}"
    );
}

#[test]
fn a_file_can_be_read_back_through_cat() {
    let remote = remote!();
    fs::write(remote.root.join("a.txt"), b"hello rclone").unwrap();

    let mut fs_ = remote.fs();
    let mut buffer = Vec::new();
    let bytes = fs_
        .read_into(Path::new("/a.txt"), &mut buffer)
        .expect("a read");

    assert_eq!(bytes, 12);
    assert_eq!(String::from_utf8_lossy(&buffer), "hello rclone");
}

/// `cat --offset`, which is what a resumed read would use.
#[test]
fn a_read_can_start_partway_in() {
    let remote = remote!();
    fs::write(remote.root.join("a.txt"), b"hello rclone").unwrap();

    let mut fs_ = remote.fs();
    let mut handle = fs_
        .open_read(Path::new("/a.txt"), 6)
        .expect("an offset read");

    let mut text = String::new();
    std::io::Read::read_to_string(&mut handle, &mut text).unwrap();
    handle.finish().expect("a clean finish");

    assert_eq!(text, "rclone");
}

#[test]
fn a_file_can_be_written_through_rcat() {
    let remote = remote!();
    let mut fs_ = remote.fs();

    let mut source = std::io::Cursor::new(b"written by lazysync".to_vec());
    let bytes = fs_
        .write_from(
            Path::new("/out.txt"),
            &WriteMetadata::default(),
            &mut source,
        )
        .expect("a write");

    assert_eq!(bytes, 19);
    assert_eq!(
        fs::read_to_string(remote.root.join("out.txt")).unwrap(),
        "written by lazysync",
        "checked on disk rather than through the client that wrote it"
    );
}

#[test]
fn a_directory_can_be_created_and_removed() {
    let remote = remote!();
    let mut fs_ = remote.fs();

    // rclone's mkdir creates parents, so the nested case needs no walk.
    fs_.create_dir(Path::new("/made/by/lazysync"))
        .expect("mkdir");
    assert!(remote.root.join("made/by/lazysync").is_dir());

    // Idempotent: two panes following each other both ask.
    fs_.create_dir(Path::new("/made/by/lazysync"))
        .expect("twice");

    fs_.remove_dir_all(Path::new("/made")).expect("purge");
    assert!(!remote.root.join("made").exists());
}

#[test]
fn a_file_can_be_deleted_and_renamed() {
    let remote = remote!();
    fs::write(remote.root.join("from.txt"), b"x").unwrap();

    let mut fs_ = remote.fs();
    fs_.rename(Path::new("/from.txt"), Path::new("/to.txt"))
        .expect("moveto");
    assert!(!remote.root.join("from.txt").exists());
    assert_eq!(fs::read_to_string(remote.root.join("to.txt")).unwrap(), "x");

    fs_.remove_file(Path::new("/to.txt")).expect("deletefile");
    assert!(!remote.root.join("to.txt").exists());
}

/// **The nastiest thing found in this phase**, pinned so it cannot come back.
///
/// `rclone moveto` runs its own size-and-mtime check first, and when it
/// decides the two files are already equal it skips the copy *and deletes the
/// source anyway* -- because that is what a move does. Exit code 0.
///
/// Both files here are three bytes with different contents, which is the shape
/// that triggers it. And it is not a corner case for lazysync: the executor
/// renames a finished partial file onto its final name, and a complete
/// download is *always* the same size as what it replaces once the sizes
/// match. Without `--ignore-times` a transfer would report success, delete the
/// bytes it had just fetched, and leave the stale file in place.
#[test]
fn a_rename_over_a_same_size_file_really_replaces_it() {
    let remote = remote!();
    fs::write(remote.root.join("new.txt"), b"new").unwrap();
    fs::write(remote.root.join("old.txt"), b"old").unwrap();

    let mut fs_ = remote.fs();
    assert!(
        fs_.capabilities().atomic_overwrite_rename,
        "the claim being tested"
    );

    fs_.rename(Path::new("/new.txt"), Path::new("/old.txt"))
        .expect("a replacing rename");

    assert_eq!(
        fs::read_to_string(remote.root.join("old.txt")).unwrap(),
        "new",
        "rclone skipped the move and deleted the source: without --ignore-times `moveto` \
         treats same-size files as equal, which silently loses the transferred bytes"
    );
    assert!(
        !remote.root.join("new.txt").exists(),
        "and the source should be gone, since this is a move"
    );
}

/// A whole sync through the planner and executor, with rclone as one side.
/// This is the end the whole backend exists for.
#[test]
fn a_push_to_an_rclone_remote_transfers_and_then_settles() {
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

    let remote = remote!();

    let local_dir = remote._dir.path().join("local");
    fs::create_dir_all(local_dir.join("nested")).unwrap();
    fs::write(local_dir.join("fresh.txt"), b"brand new").unwrap();
    fs::write(local_dir.join("nested/deep.txt"), b"deeper").unwrap();

    let mut local = LocalFs::new();
    let mut rc = remote.fs();
    let config = Config::default();

    let build = |local: &mut LocalFs, rc: &mut RcloneFs| {
        diff::plan(
            diff::Endpoint {
                fs: local,
                root: &local_dir,
                label: "local".into(),
            },
            diff::Endpoint {
                fs: rc,
                root: Path::new("/"),
                label: "rclone".into(),
            },
            Direction {
                source: 0,
                delete_extraneous: false,
            },
            &config,
        )
    };

    let plan = build(&mut local, &mut rc);
    assert_eq!(plan.totals().creates, 2, "{:?}", plan.rows);

    let report = executor::apply(
        &mut local,
        &local_dir,
        &mut rc,
        Path::new("/"),
        &plan,
        &Settings::transient(DeletePolicy::Remove),
    );
    assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

    assert_eq!(
        fs::read_to_string(remote.root.join("fresh.txt")).unwrap(),
        "brand new"
    );
    assert_eq!(
        fs::read_to_string(remote.root.join("nested/deep.txt")).unwrap(),
        "deeper",
        "a nested create has to bring its directory with it here too"
    );

    // And the property that matters most: a second run finds nothing to do.
    // rclone cannot set a modification time, so this only passes because the
    // comparator degrades to a size comparison for such a destination --
    // the rule phase 5's FTP work put in.
    let second = build(&mut local, &mut rc);
    assert_eq!(
        second.totals().operations(),
        0,
        "a settled tree must not re-copy: {:?}",
        second
            .rows
            .iter()
            .filter(|r| r.needs_review())
            .map(|r| (&r.path, r.action, r.reason))
            .collect::<Vec<_>>()
    );
}

/// `rclone://gdrive/Documents` has to parse into the remote and its path, so
/// a bookmark or a command-line argument can name one.
#[test]
fn an_rclone_url_parses_into_a_remote_and_path() {
    use lazysync::connection::{parse, Protocol};

    let target = parse("rclone://gdrive/Documents/notes").expect("a target");
    assert_eq!(target.protocol, Protocol::Rclone);
    assert_eq!(
        target.host, "gdrive",
        "the host field carries the remote's name"
    );
    assert_eq!(
        target.remote_path.as_deref(),
        Some(Path::new("/Documents/notes"))
    );
}

/// And the remote alone, with no path, is the remote's root.
#[test]
fn an_rclone_url_may_name_only_the_remote() {
    use lazysync::connection::{parse, Protocol};

    let target = parse("rclone://b2").expect("a target");
    assert_eq!(target.protocol, Protocol::Rclone);
    assert_eq!(target.host, "b2");
}

/// rclone keeps its own credentials, so lazysync must not ask for one: a
/// keyring entry it created would never be read.
#[test]
fn an_rclone_target_needs_no_credential_from_lazysync() {
    use lazysync::connection::Protocol;

    assert!(!Protocol::Rclone.needs_credentials());
    assert!(
        Protocol::Sftp.needs_credentials(),
        "every other protocol still does"
    );
}

/// The payoff of the whole backend, stated as a test: the protocols blocked on
/// `remotefs 1.0` ports are reachable this way instead.
#[test]
fn rclone_is_implemented_even_though_s3_and_webdav_are_not() {
    use lazysync::connection::Protocol;

    assert!(Protocol::Rclone.is_implemented());
    assert!(Protocol::Rclone.unavailable_because().is_none());

    for blocked in [Protocol::S3, Protocol::Webdav, Protocol::Smb] {
        assert!(!blocked.is_implemented(), "{blocked}");
        let why = blocked.unavailable_because().expect("a reason");
        assert!(why.len() > 10, "{why}");
    }
}
