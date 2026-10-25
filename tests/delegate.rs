//! Delegating transfers to a real `rsync`.
//!
//! The reason these exist rather than unit tests with a fake: the whole risk
//! in delegation is that the tool makes its own decisions. A mock would make
//! exactly the decisions we told it to and prove nothing. What has to be
//! verified against the real program is that it copies **only** the rows the
//! plan listed, and copies **all** of them -- including ones rsync's own
//! size-and-mtime check would otherwise skip.
//!
//! Skipped when rsync is not installed, like the protocol fixtures.

use std::{fs, path::Path};

use filetime::FileTime;
use lazysync::{
    config::{Config, Executor},
    sync::{
        delegate::{self, Endpoints, Tool, Unusable},
        diff::{self, Direction},
        external::Spec,
        plan::Plan,
        safety::DeletePolicy,
        transfer::{Settings, Transfer},
    },
    vfs::LocalFs,
};
use tempfile::TempDir;

fn write(path: &Path, content: &str, epoch: i64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
    filetime::set_file_mtime(path, FileTime::from_unix_time(epoch, 0)).unwrap();
}

struct Trees {
    _dir: TempDir,
    left: std::path::PathBuf,
    right: std::path::PathBuf,
}

fn trees() -> Trees {
    let dir = tempfile::tempdir().unwrap();
    let left = dir.path().join("left");
    let right = dir.path().join("right");
    fs::create_dir_all(&left).unwrap();
    fs::create_dir_all(&right).unwrap();
    Trees {
        _dir: dir,
        left,
        right,
    }
}

fn endpoints(t: &Trees) -> Endpoints {
    Endpoints {
        local: Some(Spec::Local(t.left.clone())),
        remote: Some(Spec::Local(t.right.clone())),
    }
}

fn push_plan(t: &Trees) -> Plan {
    let mut local = LocalFs::new();
    let mut remote = LocalFs::new();
    diff::plan(
        diff::Endpoint {
            fs: &mut local,
            root: &t.left,
            label: "local".into(),
        },
        diff::Endpoint {
            fs: &mut remote,
            root: &t.right,
            label: "remote".into(),
        },
        Direction {
            source: 0,
            delete_extraneous: true,
        },
        &Config::default(),
    )
}

/// Run a delegated transfer to completion, as the main loop does.
fn run(t: &Trees, plan: Plan, tool: Tool) -> lazysync::sync::Report {
    let mut transfer = Transfer::delegated(
        plan,
        Settings::transient(DeletePolicy::Remove),
        tool,
        &endpoints(t),
        &[],
    )
    .expect("spawning the tool");

    let mut local = LocalFs::new();
    let mut remote = LocalFs::new();
    let mut guard = 0;
    while !transfer.step(&mut local, &t.left, &mut remote, &t.right) {
        guard += 1;
        assert!(guard < 500_000, "the delegated transfer did not terminate");
    }
    transfer.report().clone()
}

macro_rules! needs_rsync {
    () => {
        if !lazysync::sync::external::available("rsync") {
            eprintln!("skipping: rsync is not installed");
            return;
        }
    };
}

/// The central property: rsync moves exactly the plan's rows and nothing else.
///
/// `extra.txt` is in the source tree but excluded from the plan. If rsync were
/// deciding for itself it would copy it, and the review step would be a lie.
#[test]
fn rsync_transfers_only_the_rows_the_plan_listed() {
    needs_rsync!();
    let t = trees();

    write(&t.left.join("wanted.txt"), "yes", 1_000_000);
    write(&t.left.join("extra.txt"), "no", 1_000_000);
    write(&t.left.join("nested/deep.txt"), "also yes", 1_000_000);

    let mut plan = push_plan(&t);
    for row in &mut plan.rows {
        if row.path == Path::new("extra.txt") {
            row.included = false;
        }
    }

    let report = run(&t, plan, Tool::Rsync);
    assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

    assert_eq!(
        fs::read_to_string(t.right.join("wanted.txt")).unwrap(),
        "yes"
    );
    assert_eq!(
        fs::read_to_string(t.right.join("nested/deep.txt")).unwrap(),
        "also yes",
        "a nested row has to bring its directory with it"
    );
    assert!(
        !t.right.join("extra.txt").exists(),
        "rsync copied a file the plan excluded -- the review step would be meaningless"
    );
}

/// **The bug that `--ignore-times` exists to prevent.**
///
/// The two sides have the same size and mtime but *different contents*. Our
/// planner was told to compare by checksum and so plans an overwrite; rsync's
/// own quick check sees size and time agree and would skip the file. The row
/// was shown as an overwrite, so it must be overwritten.
#[test]
fn rsync_does_not_second_guess_the_plan_and_skip_a_row() {
    needs_rsync!();
    let t = trees();

    // Same length, same timestamp, different bytes.
    write(&t.left.join("a.txt"), "AAAA", 1_000_000);
    write(&t.right.join("a.txt"), "BBBB", 1_000_000);

    let config: Config = toml::from_str("[compare]\nmethod = \"checksum\"\n").unwrap();
    let mut local = LocalFs::new();
    let mut remote = LocalFs::new();
    let plan = diff::plan(
        diff::Endpoint {
            fs: &mut local,
            root: &t.left,
            label: "local".into(),
        },
        diff::Endpoint {
            fs: &mut remote,
            root: &t.right,
            label: "remote".into(),
        },
        Direction {
            source: 0,
            delete_extraneous: false,
        },
        &config,
    );

    // Force the row to be an overwrite regardless of how the planner compared,
    // since the point here is rsync's behaviour, not the comparator's.
    let mut plan = plan;
    for row in &mut plan.rows {
        if row.path == Path::new("a.txt") {
            row.action = lazysync::sync::Action::Overwrite;
            row.included = true;
        }
    }

    let report = run(&t, plan, Tool::Rsync);
    assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

    assert_eq!(
        fs::read_to_string(t.right.join("a.txt")).unwrap(),
        "AAAA",
        "rsync skipped a row the plan said to overwrite: without --ignore-times its own \
         size+mtime check silently disagrees with ours"
    );
}

/// Deletions stay native even when a tool is moving the bytes, so the backup
/// policy and the safety rails keep applying unchanged.
#[test]
fn deletions_are_performed_natively_alongside_a_delegated_transfer() {
    needs_rsync!();
    let t = trees();

    write(&t.left.join("keep.txt"), "new", 2_000_000);
    write(&t.right.join("obsolete.txt"), "old", 1_000_000);

    let plan = push_plan(&t);
    assert_eq!(
        plan.totals().deletes,
        1,
        "the mirror should plan a deletion"
    );

    let report = run(&t, plan, Tool::Rsync);
    assert_eq!(report.failures().count(), 0, "{:?}", report.applied);

    assert_eq!(fs::read_to_string(t.right.join("keep.txt")).unwrap(), "new");
    assert!(
        !t.right.join("obsolete.txt").exists(),
        "the native engine still has to perform the deletions"
    );

    // Both kinds of row reach the report, so the progress screen and the audit
    // log cannot tell which engine moved what.
    let paths: Vec<_> = report
        .applied
        .iter()
        .map(|a| a.path.display().to_string())
        .collect();
    assert!(paths.contains(&"keep.txt".to_string()), "{paths:?}");
    assert!(paths.contains(&"obsolete.txt".to_string()), "{paths:?}");
}

/// Every delegated path lands in the report, so byte totals and the summary
/// are the same shape as a native run's.
#[test]
fn a_delegated_run_reports_every_path_and_its_bytes() {
    needs_rsync!();
    let t = trees();

    write(&t.left.join("one.txt"), "12345", 1_000_000);
    write(&t.left.join("two.txt"), "123", 1_000_000);

    let report = run(&t, push_plan(&t), Tool::Rsync);

    assert_eq!(report.succeeded(), 2, "{:?}", report.applied);
    assert_eq!(
        report.bytes, 8,
        "5 + 3 bytes, credited from the plan rather than from the tool's own totals"
    );
    assert!(
        report.summary().contains("2 applied"),
        "{}",
        report.summary()
    );
}

/// And `native` means native, even with rsync sitting right there.
#[test]
fn native_is_honoured_even_when_a_tool_is_available() {
    needs_rsync!();
    let t = trees();
    write(&t.left.join("a.txt"), "x", 1_000_000);

    let (tool, why) = delegate::choose(Executor::Native, &push_plan(&t), &endpoints(&t));
    assert!(tool.is_none());
    assert!(why.is_none(), "native is a choice, not a fallback: {why:?}");
}

/// A named tool that cannot run has to say why, rather than quietly doing
/// something else.
#[test]
fn a_named_tool_that_cannot_run_explains_itself() {
    let t = trees();
    write(&t.left.join("a.txt"), "x", 1_000_000);

    // No addressable remote side.
    let unreachable = Endpoints {
        local: Some(Spec::Local(t.left.clone())),
        remote: None,
    };

    let (tool, why) = delegate::choose(Executor::Rsync, &push_plan(&t), &unreachable);
    assert!(tool.is_none());

    let why = why.expect("a reason");
    assert!(
        matches!(
            why,
            Unusable::CannotAddress { .. } | Unusable::NotInstalled(_)
        ),
        "{why:?}"
    );
    assert!(why.to_string().len() > 10, "{why}");
}

/// **`auto` means the native engine**, even with rsync installed and usable.
///
/// Not a limitation but a decision, and one made by measurement: the moment
/// `auto` started delegating, three existing tests failed. A delegated
/// transfer does not re-check the destination before writing (so a file that
/// changed after the plan was built gets clobbered instead of skipped), has
/// aggregate rather than per-file progress, and does not preserve a
/// keep-both conflict's displaced copy. Those are trades a user may accept
/// when they ask for rsync by name. Making them silently, under a setting
/// that promises "the best available", is not defensible.
#[test]
fn auto_does_not_delegate_even_when_a_tool_is_available() {
    needs_rsync!();
    let t = trees();
    write(&t.left.join("a.txt"), "x", 1_000_000);

    let (tool, why) = delegate::choose(Executor::Auto, &push_plan(&t), &endpoints(&t));
    assert!(
        tool.is_none(),
        "auto must keep the guarantees only the native engine provides"
    );
    assert!(why.is_none(), "and it is a choice, not a fallback: {why:?}");

    // Named explicitly, the same plan does delegate.
    let (named, _) = delegate::choose(Executor::Rsync, &push_plan(&t), &endpoints(&t));
    assert_eq!(named, Some(Tool::Rsync), "asking by name still works");
}

/// A conflict resolved "keep both" is never delegated, even by name.
///
/// The displaced copy is preserved by lazysync's own write path. A tool would
/// overwrite it, losing the file the user deliberately chose to keep -- so
/// this is refused rather than offered as a trade.
#[test]
fn keep_both_is_refused_rather_than_losing_the_displaced_copy() {
    needs_rsync!();
    let t = trees();

    let mut plan = push_plan(&t);
    plan.rows.push(lazysync::sync::plan::conflict_row(
        "both.txt".into(),
        None,
        None,
        lazysync::sync::Reason::BothChanged,
        lazysync::sync::Method::SizeMtime,
    ));
    if let Some(row) = plan.rows.last_mut() {
        row.resolution = Some(lazysync::sync::Resolution::KeepBoth);
    }

    let why = delegate::usable(Tool::Rsync, &plan, &endpoints(&t)).unwrap_err();
    assert_eq!(why, Unusable::WouldLoseAKeptCopy);
    assert!(
        why.to_string().contains("keep both"),
        "the reason has to name what it is protecting: {why}"
    );
}
