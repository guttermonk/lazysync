//! Walking two trees and building a [`Plan`].
//!
//! Depth-first, listing both sides of each directory and merging by name. The
//! walk is deliberately tolerant: a directory it cannot read becomes an entry
//! in [`Plan::unreadable`] rather than aborting the whole plan, because a plan
//! covering the readable part of a tree is useful and a *silently* partial one
//! is dangerous. The confirmation window shows that list, so an unreadable
//! directory can never be mistaken for an empty one.
//!
//! Symlinks are not transferred. `Vfs` has no "create a symlink" operation
//! yet, and the two alternatives -- following the link and copying its target
//! as a regular file, or quietly dropping it -- both change the tree in ways
//! the user did not ask for. They appear as skipped rows with a caveat
//! instead, which is visible and honest.

use std::path::{Path, PathBuf};

use super::{
    compare::Comparator,
    filters::Filters,
    plan::{self, Plan, Row},
};
use crate::{
    config::Config,
    vfs::{Entry, EntryKind, Vfs, VfsError},
};

/// One side of a sync: a backend, a root, and a name for the window.
///
/// Grouped rather than passed as three parallel arguments, because a planner
/// taking `(source_fs, source_root, source_label, dest_fs, dest_root,
/// dest_label, ..)` is one transposed pair away from syncing the wrong
/// direction -- and the compiler could not tell you.
pub struct Endpoint<'a> {
    pub fs: &'a mut dyn Vfs,
    pub root: &'a Path,
    /// `local` or `remote`, for the confirmation window's header.
    pub label: String,
}

/// Which way files move, and whether extraneous ones are removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Direction {
    /// Index of the pane files come from: 0 local, 1 remote.
    pub source: usize,
    /// Whether files present only on the destination are deleted.
    pub delete_extraneous: bool,
}

impl Direction {
    pub const fn dest(self) -> usize {
        1 - self.source
    }
}

/// How deep the walk will go before giving up.
///
/// A guard against a symlink loop on a backend that resolves them server-side,
/// or a filesystem that reports a directory as its own child. Hitting it is
/// recorded in [`Plan::unreadable`] rather than silently truncating.
const MAX_DEPTH: usize = 64;

/// Build a plan for syncing `source_root` onto `dest_root`.
pub fn plan(
    source: Endpoint<'_>,
    dest: Endpoint<'_>,
    direction: Direction,
    config: &Config,
) -> Plan {
    plan_with_filters(source, dest, direction, config, &Filters::default())
}

/// As [`plan`], with exclude rules applied.
pub fn plan_with_filters(
    source: Endpoint<'_>,
    dest: Endpoint<'_>,
    direction: Direction,
    config: &Config,
    filters: &Filters,
) -> Plan {
    let Endpoint {
        fs: source,
        root: source_root,
        label: source_label,
    } = source;
    let Endpoint {
        fs: dest,
        root: dest_root,
        label: dest_label,
    } = dest;

    let comparator = Comparator::new(&config.compare, source.capabilities(), dest.capabilities());
    let policy = super::safety::DeletePolicy::from_config(&config.safety);

    let mut built = Plan {
        source_root: source_root.to_path_buf(),
        dest_root: dest_root.to_path_buf(),
        source_label,
        dest_label,
        method: comparator.method(),
        tolerance_secs: comparator.tolerance_secs(),
        ..Plan::default()
    };

    // Whether the destination can preserve what a transfer implies. Computed
    // once: it is a property of the backend, not of each file, and a row that
    // silently loses its timestamp makes the *next* sync re-copy it.
    let dest_caps = dest.capabilities();
    let mtime_caveat = (!dest_caps.set_mtime && source.capabilities().mtime).then_some(
        "the destination cannot set timestamps, so the next sync will see this as changed",
    );
    let mode_caveat = (!dest_caps.mode && source.capabilities().mode)
        .then_some("the destination has no permission bits");

    walk(
        source,
        source_root,
        dest,
        dest_root,
        Path::new(""),
        0,
        direction,
        &comparator,
        config,
        mtime_caveat,
        mode_caveat,
        &policy,
        filters,
        &mut built,
    );

    built.sort();
    built
}

// The recursive worker genuinely needs all of this threaded through it;
// bundling it into a struct would only move the same fields somewhere else.
#[allow(clippy::too_many_arguments)]
fn walk(
    source: &mut dyn Vfs,
    source_root: &Path,
    dest: &mut dyn Vfs,
    dest_root: &Path,
    relative: &Path,
    depth: usize,
    direction: Direction,
    comparator: &Comparator,
    config: &Config,
    mtime_caveat: Option<&'static str>,
    mode_caveat: Option<&'static str>,
    policy: &super::safety::DeletePolicy,
    filters: &Filters,
    built: &mut Plan,
) {
    if depth > MAX_DEPTH {
        built.unreadable.push((
            relative.to_path_buf(),
            format!("stopped at {MAX_DEPTH} levels deep; a symlink loop?"),
        ));
        return;
    }

    let source_dir = source_root.join(relative);
    let dest_dir = dest_root.join(relative);

    let source_entries = match source.list(&source_dir) {
        Ok(entries) => entries,
        Err(e) => {
            // The source side being unreadable means this subtree cannot be
            // planned at all, so it is recorded and skipped rather than
            // producing rows that would delete the destination's copy.
            built
                .unreadable
                .push((relative.to_path_buf(), format!("source: {e}")));
            return;
        }
    };

    // A destination directory that does not exist yet is not an error: every
    // source entry under it is simply a creation.
    let dest_entries = match dest.list(&dest_dir) {
        Ok(entries) => entries,
        Err(e) if e.is_not_found() => Vec::new(),
        Err(e) => {
            built
                .unreadable
                .push((relative.to_path_buf(), format!("destination: {e}")));
            // Carry on with an empty destination listing. Planning creations
            // for a directory that could not be read is safe -- a create that
            // finds a file already there fails loudly -- whereas planning
            // *deletions* from a listing we do not have would be catastrophic,
            // so that is suppressed below.
            Vec::new()
        }
    };
    let dest_readable = !built
        .unreadable
        .iter()
        .any(|(path, why)| path == relative && why.starts_with("destination:"));

    for entry in &source_entries {
        if skip_hidden(entry, config) {
            continue;
        }

        let child = relative.join(&entry.name);

        // The destination's backup directory is lazysync's own bookkeeping,
        // not content. Syncing it would copy old versions back as if they
        // were real files.
        if super::safety::is_backup_path(policy, &child) {
            continue;
        }

        // Exclude rules. Applied here rather than after planning, so an
        // excluded directory is never descended into at all -- on a remote
        // that is a saved round trip per level.
        let is_dir = matches!(entry.kind, EntryKind::Dir) || entry.symlink_to_dir;
        if filters.excludes(&child, is_dir) {
            continue;
        }

        // The guard against a listing that somehow names a path outside the
        // root. A backend is not supposed to do this; if one does, writing
        // there would escape the sync root entirely.
        if plan::escapes_root(source_root, &child) {
            built.unreadable.push((
                child,
                "entry name escapes the sync root; refusing to plan it".into(),
            ));
            continue;
        }

        let counterpart = dest_entries.iter().find(|d| d.name == entry.name);

        match (entry.kind, counterpart.map(|d| d.kind)) {
            // Symlinks: visible, and deliberately not transferred.
            (EntryKind::Symlink, _) => {
                built.rows.push(Row {
                    caveat: Some("symlink, not transferred (arrives with phase 6)"),
                    ..plan::compare_row(
                        child,
                        entry,
                        counterpart.unwrap_or(entry),
                        super::compare::Reason::SameSizeNoTimestamps,
                        comparator.method(),
                    )
                });
            }

            // A directory on both sides: nothing to transfer, just recurse.
            (EntryKind::Dir, Some(EntryKind::Dir)) => {
                walk(
                    source,
                    source_root,
                    dest,
                    dest_root,
                    &child,
                    depth + 1,
                    direction,
                    comparator,
                    config,
                    mtime_caveat,
                    mode_caveat,
                    policy,
                    filters,
                    built,
                );
            }

            // A directory that does not exist on the destination: create it,
            // then plan its contents.
            (EntryKind::Dir, None) => {
                built
                    .rows
                    .push(plan::create_row(child.clone(), entry, comparator.method()));
                walk(
                    source,
                    source_root,
                    dest,
                    dest_root,
                    &child,
                    depth + 1,
                    direction,
                    comparator,
                    config,
                    mtime_caveat,
                    mode_caveat,
                    policy,
                    filters,
                    built,
                );
            }

            // A directory where the destination has a file, or vice versa.
            // Replacing one with the other is a bigger change than an
            // overwrite, so it is flagged rather than performed quietly.
            (EntryKind::Dir, Some(_)) | (EntryKind::File, Some(EntryKind::Dir)) => {
                built.rows.push(Row {
                    caveat: Some(
                        "type differs: one side is a directory. Not transferred -- remove the \
                         destination by hand if this is what you want",
                    ),
                    included: false,
                    ..plan::compare_row(
                        child,
                        entry,
                        counterpart.unwrap_or(entry),
                        super::compare::Reason::SizeDiffers,
                        comparator.method(),
                    )
                });
            }

            // The source is a file and the destination is a symlink.
            // Writing through it would modify whatever it points at, which
            // may be outside the sync root entirely, so it is flagged and
            // left alone.
            (EntryKind::File, Some(EntryKind::Symlink)) => {
                built.rows.push(Row {
                    caveat: Some(
                        "the destination is a symlink; writing through it could modify a file \
                         outside the sync root. Not transferred",
                    ),
                    included: false,
                    ..plan::compare_row(
                        child,
                        entry,
                        counterpart.unwrap_or(entry),
                        super::compare::Reason::SizeDiffers,
                        comparator.method(),
                    )
                });
            }

            // The ordinary cases.
            (EntryKind::File, None) => {
                built
                    .rows
                    .push(plan::create_row(child, entry, comparator.method()));
            }
            (EntryKind::File, Some(EntryKind::File)) => {
                let other = counterpart.expect("matched Some above");
                // Checksums are not computed here: hashing both sides of a
                // whole tree while the UI waits would freeze it. The window
                // offers `c` to re-check selected rows, which is where the
                // reading belongs.
                let reason = comparator.compare(entry, other, None);

                let caveat = if reason.differs() {
                    mtime_caveat.or(mode_caveat)
                } else {
                    None
                };

                built.rows.push(Row {
                    caveat,
                    ..plan::compare_row(child, entry, other, reason, comparator.method())
                });
            }
        }
    }

    // Extraneous entries on the destination. Only in a mirror, and only when
    // the destination listing was actually read: deleting based on a listing
    // that failed would remove files for no reason other than a transient
    // error.
    if direction.delete_extraneous && dest_readable {
        for entry in &dest_entries {
            if skip_hidden(entry, config) {
                continue;
            }
            if source_entries.iter().any(|s| s.name == entry.name) {
                continue;
            }

            let child = relative.join(&entry.name);

            // Never delete the backup directory. It exists precisely because
            // deletions should be recoverable, and since it lives inside the
            // destination root the next mirror would otherwise see it as
            // extraneous and remove it -- destroying every backup it holds.
            if super::safety::is_backup_path(policy, &child) {
                continue;
            }

            // An excluded path is not lazysync's business on either side. A
            // mirror that deleted the destination's `target/` because the
            // source's was filtered out would be deleting files the user
            // explicitly told it to ignore.
            let is_dir = matches!(entry.kind, EntryKind::Dir) || entry.symlink_to_dir;
            if filters.excludes(&child, is_dir) {
                continue;
            }
            built
                .rows
                .push(plan::delete_row(child, entry, comparator.method()));
        }
    }
}

/// Whether an entry is excluded because dotfiles are hidden.
///
/// Tied to the browser's own `show_hidden` so that what you see is what gets
/// synced. A sync that silently included files the panes were not showing
/// would be a plan you could not review.
fn skip_hidden(entry: &Entry, config: &Config) -> bool {
    !config.show_hidden && entry.is_hidden()
}

/// Re-stat one row's two sides.
///
/// Used before applying, so that a plan built a minute ago is checked against
/// the tree as it is now. Returns the fresh entries, or an error naming what
/// changed.
pub fn recheck(
    source: &mut dyn Vfs,
    source_root: &Path,
    dest: &mut dyn Vfs,
    dest_root: &Path,
    relative: &Path,
) -> Result<(Option<Entry>, Option<Entry>), VfsError> {
    let from = match source.stat(&source_root.join(relative)) {
        Ok(entry) => Some(entry),
        Err(e) if e.is_not_found() => None,
        Err(e) => return Err(e),
    };
    let to = match dest.stat(&dest_root.join(relative)) {
        Ok(entry) => Some(entry),
        Err(e) if e.is_not_found() => None,
        Err(e) => return Err(e),
    };
    Ok((from, to))
}

/// Paths whose destination changed since the plan was built.
pub fn stale_rows(plan: &Plan, checked: &[(PathBuf, Option<Entry>)]) -> Vec<PathBuf> {
    let mut stale = Vec::new();

    for (path, current) in checked {
        let Some(row) = plan.rows.iter().find(|r| r.path == *path) else {
            continue;
        };

        let planned = row.dest;
        let now = current.as_ref().map(super::plan::Side::from_entry);

        if planned != now {
            stale.push(path.clone());
        }
    }

    stale
}

#[cfg(test)]
mod tests {
    use std::fs;

    use filetime::FileTime;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::{CompareMethod, Config},
        sync::plan::{Action, Section},
        vfs::LocalFs,
    };

    /// Write `content` to `path`, with a fixed mtime so comparisons are
    /// deterministic rather than depending on how fast the test ran.
    fn write(path: &Path, content: &str, epoch: i64) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
        filetime::set_file_mtime(path, FileTime::from_unix_time(epoch, 0)).unwrap();
    }

    struct Trees {
        _dir: TempDir,
        left: PathBuf,
        right: PathBuf,
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

    fn run(t: &Trees, delete_extraneous: bool, config: &Config) -> Plan {
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        plan(
            Endpoint {
                fs: &mut source,
                root: &t.left,
                label: "local".into(),
            },
            Endpoint {
                fs: &mut dest,
                root: &t.right,
                label: "remote".into(),
            },
            Direction {
                source: 0,
                delete_extraneous,
            },
            config,
        )
    }

    fn paths(plan: &Plan, action: Action) -> Vec<String> {
        let mut found: Vec<String> = plan
            .rows
            .iter()
            .filter(|r| r.action == action)
            .map(|r| r.path.display().to_string())
            .collect();
        found.sort();
        found
    }

    #[test]
    fn a_new_file_is_planned_as_a_create() {
        let t = trees();
        write(&t.left.join("a.txt"), "hello", 1000);

        let plan = run(&t, false, &Config::default());

        assert_eq!(paths(&plan, Action::Create), ["a.txt"]);
        assert_eq!(plan.totals().bytes_out, 5);
        assert!(plan.can_apply());
    }

    #[test]
    fn an_identical_file_is_skipped() {
        let t = trees();
        write(&t.left.join("a.txt"), "hello", 1000);
        write(&t.right.join("a.txt"), "hello", 1000);

        let plan = run(&t, false, &Config::default());

        assert_eq!(plan.count(Section::Skipped), 1);
        assert_eq!(plan.totals().operations(), 0);
        assert!(plan.is_empty(), "nothing to do means nothing to confirm");
    }

    #[test]
    fn a_changed_file_is_an_overwrite_with_both_sides_shown() {
        let t = trees();
        write(&t.left.join("a.txt"), "hello world", 2000);
        write(&t.right.join("a.txt"), "hello", 1000);

        let plan = run(&t, false, &Config::default());
        let row = plan
            .rows
            .iter()
            .find(|r| r.path == Path::new("a.txt"))
            .unwrap();

        assert_eq!(row.action, Action::Overwrite);
        assert_eq!(row.source.unwrap().size, 11);
        assert_eq!(
            row.dest.unwrap().size,
            5,
            "the window needs the destination's size to show what is being replaced"
        );
        assert!(row.source.unwrap().mtime.is_some());
        assert!(row.dest.unwrap().mtime.is_some());
    }

    /// The case the window colours red.
    #[test]
    fn a_newer_destination_is_flagged() {
        let t = trees();
        write(&t.left.join("notes.md"), "aaaaa", 1000);
        write(&t.right.join("notes.md"), "bbbbb", 2000);

        let plan = run(&t, false, &Config::default());
        let row = &plan.rows[0];

        assert_eq!(row.action, Action::Overwrite);
        assert!(
            row.clobbers_newer(),
            "same size but a newer destination still means data loss"
        );
        assert_eq!(plan.totals().clobbers_newer, 1);
    }

    #[test]
    fn extraneous_files_are_only_deleted_in_a_mirror() {
        let t = trees();
        write(&t.left.join("keep"), "x", 1000);
        write(&t.right.join("keep"), "x", 1000);
        write(&t.right.join("extra"), "y", 1000);

        let push = run(&t, false, &Config::default());
        assert_eq!(push.count(Section::Delete), 0, "push must not delete");

        let mirror = run(&t, true, &Config::default());
        assert_eq!(paths(&mirror, Action::Delete), ["extra"]);
        assert_eq!(mirror.totals().bytes_deleted, 1);
    }

    #[test]
    fn nested_directories_are_walked_and_created() {
        let t = trees();
        write(&t.left.join("src/api/handler.rs"), "fn main() {}", 1000);

        let plan = run(&t, false, &Config::default());

        assert_eq!(paths(&plan, Action::Create), ["src/api/handler.rs"]);
        assert_eq!(paths(&plan, Action::CreateDir), ["src", "src/api"]);
        assert_eq!(
            plan.required_directories(),
            vec![PathBuf::from("src"), PathBuf::from("src/api")]
        );
    }

    #[test]
    fn dotfiles_follow_the_browsers_own_setting() {
        let t = trees();
        write(&t.left.join(".env"), "SECRET=1", 1000);
        write(&t.left.join("visible"), "x", 1000);

        let hidden = run(&t, false, &Config::default());
        assert_eq!(
            paths(&hidden, Action::Create),
            ["visible"],
            "a sync must not include files the panes were not showing"
        );

        let shown = run(
            &t,
            false,
            &Config {
                show_hidden: true,
                ..Config::default()
            },
        );
        assert_eq!(paths(&shown, Action::Create), [".env", "visible"]);
    }

    /// A directory facing a file is a bigger change than an overwrite, so it
    /// is excluded by default and labelled.
    #[test]
    fn a_type_mismatch_is_flagged_and_excluded() {
        let t = trees();
        fs::create_dir_all(t.left.join("thing")).unwrap();
        write(&t.left.join("thing/inner"), "x", 1000);
        write(&t.right.join("thing"), "i am a file", 1000);

        let plan = run(&t, false, &Config::default());
        let row = plan
            .rows
            .iter()
            .find(|r| r.path == Path::new("thing"))
            .unwrap();

        assert!(!row.included, "it should not be applied without a decision");
        assert!(row.caveat.unwrap().contains("type differs"));
    }

    #[test]
    fn symlinks_are_shown_but_not_transferred() {
        let t = trees();
        write(&t.left.join("real"), "x", 1000);
        #[cfg(unix)]
        std::os::unix::fs::symlink("real", t.left.join("link")).unwrap();

        let plan = run(&t, false, &Config::default());

        #[cfg(unix)]
        {
            let row = plan
                .rows
                .iter()
                .find(|r| r.path == Path::new("link"))
                .unwrap();
            assert_eq!(row.action, Action::Skip);
            assert!(row.caveat.unwrap().contains("symlink"));
            assert_eq!(row.bytes(), 0);
        }
        assert_eq!(paths(&plan, Action::Create), ["real"]);
    }

    /// An unreadable directory must be reported, never silently treated as
    /// empty -- on a mirror that would plan the deletion of everything in it.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_source_directory_is_reported_not_skipped_silently() {
        use std::os::unix::fs::PermissionsExt;

        let t = trees();
        let locked = t.left.join("locked");
        fs::create_dir_all(&locked).unwrap();
        write(&locked.join("inside"), "x", 1000);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let plan = run(&t, true, &Config::default());

        // Restore before any assertion can fail and leak the mode.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            plan.unreadable
                .iter()
                .any(|(p, _)| p == Path::new("locked")),
            "an unreadable directory has to appear in the plan: {:?}",
            plan.unreadable
        );
        // Strictly deeper than `locked`. The directory's own CreateDir row is
        // correct and expected -- it exists on the source, so mirroring its
        // existence is right; only its *contents* are unknown. `starts_with`
        // alone would match that row, since Path compares whole components and
        // "locked" does start with "locked/".
        assert!(
            !plan
                .rows
                .iter()
                .any(|r| r.path.components().count() > 1 && r.path.starts_with("locked")),
            "nothing inside an unreadable directory should be planned: {:?}",
            plan.rows.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
    }

    /// Deletions must never be planned from a destination listing that failed
    /// to read.
    #[cfg(unix)]
    #[test]
    fn deletions_are_suppressed_when_the_destination_cannot_be_listed() {
        use std::os::unix::fs::PermissionsExt;

        let t = trees();
        fs::create_dir_all(t.left.join("d")).unwrap();
        write(&t.left.join("d/keep"), "x", 1000);

        let locked = t.right.join("d");
        fs::create_dir_all(&locked).unwrap();
        write(&locked.join("victim"), "y", 1000);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let plan = run(&t, true, &Config::default());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            plan.count(Section::Delete),
            0,
            "a transient read failure must not become a deletion"
        );
        assert!(plan
            .unreadable
            .iter()
            .any(|(_, why)| why.contains("destination")));
    }

    #[test]
    fn the_plan_records_how_it_compared() {
        let t = trees();
        write(&t.left.join("a"), "x", 1000);

        let plan = run(&t, false, &Config::default());
        assert_eq!(plan.method, super::super::compare::Method::SizeMtime);
        assert!(plan.tolerance_secs >= 1);

        let size_only = run(
            &t,
            false,
            &Config {
                compare: crate::config::Compare {
                    method: CompareMethod::SizeOnly,
                    ..Default::default()
                },
                ..Config::default()
            },
        );
        assert_eq!(size_only.method, super::super::compare::Method::SizeOnly);
    }

    /// Timestamps within the tolerance must not produce work, which is the
    /// whole point of having one.
    #[test]
    fn a_one_second_skew_does_not_produce_an_overwrite() {
        let t = trees();
        write(&t.left.join("a"), "hello", 1001);
        write(&t.right.join("a"), "hello", 1000);

        let plan = run(&t, false, &Config::default());
        assert_eq!(
            plan.totals().operations(),
            0,
            "a 1s skew is inside the default 2s tolerance"
        );
    }

    #[test]
    fn an_empty_source_against_a_populated_destination_plans_nothing_without_mirror() {
        let t = trees();
        write(&t.right.join("only-there"), "x", 1000);

        let plan = run(&t, false, &Config::default());
        assert!(plan.is_empty());
        assert_eq!(plan.rows.len(), 0);
    }

    #[test]
    fn rechecking_notices_a_destination_that_changed() {
        let t = trees();
        write(&t.left.join("a"), "hello", 2000);
        write(&t.right.join("a"), "old", 1000);

        let plan = run(&t, false, &Config::default());

        // Someone else writes to the destination after the plan was built.
        write(&t.right.join("a"), "changed underneath", 3000);

        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        let (_, now) = recheck(&mut source, &t.left, &mut dest, &t.right, Path::new("a")).unwrap();

        let stale = stale_rows(&plan, &[(PathBuf::from("a"), now)]);
        assert_eq!(
            stale,
            vec![PathBuf::from("a")],
            "applying a plan against a tree that moved under it has to be detectable"
        );
    }

    #[test]
    fn rechecking_an_unchanged_row_reports_nothing_stale() {
        let t = trees();
        write(&t.left.join("a"), "hello", 2000);
        write(&t.right.join("a"), "old", 1000);

        let plan = run(&t, false, &Config::default());

        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        let (_, now) = recheck(&mut source, &t.left, &mut dest, &t.right, Path::new("a")).unwrap();

        assert!(stale_rows(&plan, &[(PathBuf::from("a"), now)]).is_empty());
    }

    /// Without this, the second mirror run deletes the backups the first one
    /// made -- destroying exactly the safety net that justifies deleting at
    /// all.
    #[test]
    fn the_backup_directory_is_neither_synced_nor_deleted() {
        let t = trees();
        write(&t.left.join("keep"), "x", 1000);
        write(&t.right.join("keep"), "x", 1000);
        // What a previous mirror run left behind.
        write(
            &t.right.join(".lazysync-backup/old"),
            "a previous version",
            500,
        );

        let plan = run(&t, true, &Config::default());

        assert_eq!(
            plan.count(Section::Delete),
            0,
            "the backup directory must not be planned for deletion: {:?}",
            plan.rows.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
        assert!(
            !plan
                .rows
                .iter()
                .any(|r| r.path.starts_with(".lazysync-backup")),
            "and it must not appear as content either"
        );
    }

    /// With deletions permanent there is no backup directory, so a path of
    /// that name is ordinary content and must be synced.
    ///
    /// `show_hidden` is on and the directory is not dotted, to isolate the
    /// backup-path rule from the dotfile filter -- the default
    /// `.lazysync-backup` is hidden anyway, so a test using it would pass
    /// whatever this rule did.
    #[test]
    fn a_backup_named_path_is_ordinary_content_when_deletions_are_permanent() {
        let t = trees();
        let config = Config {
            show_hidden: true,
            safety: crate::config::Safety {
                backup_dir: String::new(),
                ..Default::default()
            },
            ..Config::default()
        };
        write(&t.left.join("backups/thing"), "mine", 1000);

        let plan = run(&t, false, &config);
        assert!(
            plan.rows
                .iter()
                .any(|r| r.path == Path::new("backups/thing")),
            "{:?}",
            plan.rows.iter().map(|r| &r.path).collect::<Vec<_>>()
        );
    }

    /// And the converse: a *configured* backup directory is excluded even
    /// when it is not hidden and dotfiles are being shown. Without this the
    /// rule would only ever be doing what the dotfile filter already did.
    #[test]
    fn a_non_hidden_backup_directory_is_still_excluded() {
        let t = trees();
        let config = Config {
            show_hidden: true,
            safety: crate::config::Safety {
                backup_dir: "backups".into(),
                ..Default::default()
            },
            ..Config::default()
        };
        write(&t.left.join("keep"), "x", 1000);
        write(&t.right.join("keep"), "x", 1000);
        write(&t.right.join("backups/old"), "a previous version", 500);

        let plan = run(&t, true, &config);

        assert_eq!(plan.count(Section::Delete), 0, "{:?}", plan.rows);
        assert!(!plan.rows.iter().any(|r| r.path.starts_with("backups")));
    }

    /// Filters are what make "advanced syncing" mean more than pushing
    /// `target/` to a web server every time.
    #[test]
    fn excluded_paths_are_not_planned() {
        let t = trees();
        write(&t.left.join("src/main.rs"), "code", 1000);
        write(&t.left.join("target/debug/binary"), "huge", 1000);
        write(&t.left.join("notes.log"), "noise", 1000);

        let filters = Filters::parse(&["target/".to_owned(), "*.log".to_owned()]).unwrap();
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        let plan = plan_with_filters(
            Endpoint {
                fs: &mut source,
                root: &t.left,
                label: "local".into(),
            },
            Endpoint {
                fs: &mut dest,
                root: &t.right,
                label: "remote".into(),
            },
            Direction {
                source: 0,
                delete_extraneous: false,
            },
            &Config::default(),
            &filters,
        );

        let planned: Vec<String> = plan
            .rows
            .iter()
            .map(|r| r.path.display().to_string())
            .collect();

        assert!(planned.contains(&"src/main.rs".to_string()), "{planned:?}");
        assert!(
            !planned.iter().any(|p| p.starts_with("target")),
            "an excluded directory must not be descended into: {planned:?}"
        );
        assert!(!planned.contains(&"notes.log".to_string()), "{planned:?}");
    }

    /// A mirror must not delete the destination's copy of something the user
    /// filtered out -- that would be deleting files they explicitly told it
    /// to ignore.
    #[test]
    fn a_mirror_does_not_delete_excluded_paths_from_the_destination() {
        let t = trees();
        write(&t.left.join("keep"), "x", 1000);
        write(&t.right.join("keep"), "x", 1000);
        write(&t.right.join("target/debug/their-build"), "theirs", 1000);
        write(&t.right.join("their.log"), "theirs", 1000);

        let filters = Filters::parse(&["target/".to_owned(), "*.log".to_owned()]).unwrap();
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        let plan = plan_with_filters(
            Endpoint {
                fs: &mut source,
                root: &t.left,
                label: "local".into(),
            },
            Endpoint {
                fs: &mut dest,
                root: &t.right,
                label: "remote".into(),
            },
            Direction {
                source: 0,
                delete_extraneous: true,
            },
            &Config::default(),
            &filters,
        );

        assert_eq!(
            plan.count(Section::Delete),
            0,
            "excluded paths are not lazysync's business on either side: {:?}",
            plan.rows
                .iter()
                .filter(|r| r.action == Action::Delete)
                .map(|r| &r.path)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn no_filters_plans_everything() {
        let t = trees();
        write(&t.left.join("target/x"), "a", 1000);
        write(&t.left.join("b.log"), "b", 1000);

        let plan = run(&t, false, &Config::default());
        assert!(plan.rows.iter().any(|r| r.path.starts_with("target")));
        assert!(plan.rows.iter().any(|r| r.path == Path::new("b.log")));
    }

    #[test]
    fn direction_flips_the_destination() {
        assert_eq!(
            Direction {
                source: 0,
                delete_extraneous: false
            }
            .dest(),
            1
        );
        assert_eq!(
            Direction {
                source: 1,
                delete_extraneous: false
            }
            .dest(),
            0
        );
    }
}
