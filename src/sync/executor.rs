//! Applying a plan.
//!
//! Sequential and synchronous, which is phase 3's deliberate limit: the
//! transfer queue, parallelism, resume and progress reporting arrive in phase
//! 4. What is here already is the part that cannot be retrofitted safely --
//! every file is written to a partial name and renamed into place, so an
//! interrupted sync never leaves a truncated file where a good one used to be.
//!
//! The executor applies exactly the rows the confirmation window showed. It
//! re-checks each destination immediately before touching it, because a plan
//! is a snapshot and the tree may have moved on; a row whose destination
//! changed is skipped and reported rather than overwritten on the strength of
//! stale information.

use std::path::{Path, PathBuf};

use super::{
    plan::{Action, Plan, Row},
    safety::DeletePolicy,
};
use crate::vfs::{Vfs, VfsError, WriteMetadata};

/// What happened to one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Copied, whether the destination existed before or not.
    ///
    /// Create and overwrite are not distinguished here because
    /// [`Applied::action`] already records which it was -- and when they were
    /// separate variants, only one of them carried a byte count, so the
    /// summary under-reported every creation.
    Transferred {
        bytes: u64,
    },
    Deleted,
    MovedToBackup {
        to: PathBuf,
    },
    /// Skipped because the destination changed after the plan was built.
    SkippedStale {
        why: String,
    },
    Failed {
        why: String,
    },
}

impl Outcome {
    pub const fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    pub const fn is_skip(&self) -> bool {
        matches!(self, Self::SkippedStale { .. })
    }
}

/// One row's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub path: PathBuf,
    pub action: Action,
    pub outcome: Outcome,
}

/// What a whole run did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub applied: Vec<Applied>,
    pub bytes: u64,
}

impl Report {
    pub fn failures(&self) -> impl Iterator<Item = &Applied> {
        self.applied.iter().filter(|a| a.outcome.is_failure())
    }

    pub fn skipped(&self) -> impl Iterator<Item = &Applied> {
        self.applied.iter().filter(|a| a.outcome.is_skip())
    }

    pub fn succeeded(&self) -> usize {
        self.applied
            .iter()
            .filter(|a| !a.outcome.is_failure() && !a.outcome.is_skip())
            .count()
    }

    /// A one-line summary for the status bar.
    pub fn summary(&self) -> String {
        let failures = self.failures().count();
        let skipped = self.skipped().count();

        let mut parts = vec![format!("{} applied", self.succeeded())];
        if self.bytes > 0 {
            parts.push(format!(
                "{} transferred",
                crate::ui::format_size(self.bytes)
            ));
        }
        if skipped > 0 {
            parts.push(format!("{skipped} skipped (changed underneath)"));
        }
        if failures > 0 {
            parts.push(format!("{failures} failed"));
        }
        parts.join(", ")
    }
}

/// Apply the included rows of `plan`.
///
/// Stops at nothing: one file failing does not abandon the rest, because a
/// half-applied sync you know the shape of is better than one that stopped
/// somewhere unspecified. Every result lands in the [`Report`].
pub fn apply(
    source: &mut dyn Vfs,
    source_root: &Path,
    dest: &mut dyn Vfs,
    dest_root: &Path,
    plan: &Plan,
    policy: &DeletePolicy,
    partial_suffix: &str,
) -> Report {
    let mut report = Report::default();

    if !plan.can_apply() {
        return report;
    }

    // Directories first, shallowest to deepest, so a create never fails for a
    // missing parent.
    for relative in plan.required_directories() {
        let target = dest_root.join(&relative);
        if let Err(e) = dest.create_dir(&target) {
            report.applied.push(Applied {
                path: relative,
                action: Action::CreateDir,
                outcome: Outcome::Failed { why: e.to_string() },
            });
        }
    }

    for row in plan.included() {
        match row.action {
            Action::CreateDir | Action::Skip => {}

            Action::Create | Action::Overwrite => {
                let outcome = transfer(source, source_root, dest, dest_root, row, partial_suffix);
                if let Outcome::Transferred { bytes } = &outcome {
                    report.bytes = report.bytes.saturating_add(*bytes);
                }
                report.applied.push(Applied {
                    path: row.path.clone(),
                    action: row.action,
                    outcome,
                });
            }

            Action::Delete => {
                let outcome = delete(dest, dest_root, row, policy);
                report.applied.push(Applied {
                    path: row.path.clone(),
                    action: row.action,
                    outcome,
                });
            }
        }
    }

    report
}

/// Copy one file, via a partial name.
fn transfer(
    source: &mut dyn Vfs,
    source_root: &Path,
    dest: &mut dyn Vfs,
    dest_root: &Path,
    row: &Row,
    partial_suffix: &str,
) -> Outcome {
    let from = source_root.join(&row.path);
    let to = dest_root.join(&row.path);

    // Re-stat the source: the plan recorded a size, and protocols that send a
    // length before the data need the *current* one. A stale size would
    // corrupt the transfer rather than merely misreport it.
    let current = match source.stat(&from) {
        Ok(entry) => entry,
        Err(e) => {
            return Outcome::Failed {
                why: format!("the source is gone: {e}"),
            }
        }
    };

    // Re-check the destination against what was planned. Overwriting on the
    // strength of a snapshot that is no longer true is how a sync destroys
    // something the user never saw in the window.
    if let Some(planned) = row.dest {
        match dest.stat(&to) {
            Ok(now) => {
                let now_side = super::plan::Side::from_entry(&now);
                if now_side != planned {
                    return Outcome::SkippedStale {
                        why: "the destination changed after the plan was built".into(),
                    };
                }
            }
            Err(e) if e.is_not_found() => {
                // Planned as an overwrite, but it is gone now. Writing it is
                // still the intent, so this proceeds.
            }
            Err(e) => return Outcome::Failed { why: e.to_string() },
        }
    }

    let metadata = WriteMetadata::from_entry(&current);
    let partial = partial_path(&to, partial_suffix);

    // Read into memory, then write. Phase 4's queue streams instead; this is
    // the simple version, and it is why phase 3 has no progress bar.
    let mut buffer = Vec::with_capacity(usize::try_from(current.size).unwrap_or(0));
    let read = match source.read_into(&from, &mut buffer) {
        Ok(count) => count,
        Err(e) => {
            return Outcome::Failed {
                why: format!("reading: {e}"),
            }
        }
    };

    // A short read means the file changed while being copied. Writing what was
    // read would put a truncated file on the destination, which is worse than
    // not copying at all.
    if read != current.size {
        return Outcome::Failed {
            why: format!(
                "the source changed while being read ({read} bytes of {})",
                current.size
            ),
        };
    }

    let mut cursor = std::io::Cursor::new(buffer);
    if let Err(e) = dest.write_from(&partial, &metadata, &mut cursor) {
        // Leave nothing behind: a stray partial file in the destination tree
        // would show up in the next plan as a file to sync back.
        let _ = dest.remove_file(&partial);
        return Outcome::Failed {
            why: format!("writing: {e}"),
        };
    }

    // The rename is what makes the whole thing safe: until it happens, the
    // destination still holds the previous good copy.
    if let Err(e) = move_into_place(dest, &partial, &to, row.dest.is_some()) {
        let _ = dest.remove_file(&partial);
        return Outcome::Failed {
            why: format!("renaming into place: {e}"),
        };
    }

    // Timestamps again after the rename. Some backends apply them to the
    // partial name and some lose them in the rename, and a destination stamped
    // "now" looks newer than its source -- which would make the next sync in
    // the other direction copy it back.
    if metadata.mtime.is_some() {
        if let Err(e) = dest.set_times(&to, &metadata) {
            tracing::debug!("could not restamp {}: {e}", to.display());
        }
    }

    Outcome::Transferred { bytes: read }
}

/// Put the finished partial file at its final name.
///
/// On a local filesystem this is one `rename(2)`, which replaces the target
/// atomically. **SFTP's rename does not**: the protocol leaves the behaviour
/// for an existing target unspecified and the OpenSSH server answers
/// `SSH_FX_PERMISSION_DENIED`, so the same call that works for a new file
/// fails for every overwrite. (The `posix-rename@openssh.com` extension fixes
/// this, but `remotefs-ssh` does not expose it.)
///
/// Where the rename cannot overwrite, the target is moved aside first and
/// removed afterwards. Three operations instead of one, and not atomic -- but
/// at every point in between, a *complete* copy of the file exists under some
/// name. A failure mid-swap is recoverable by hand, which is the property
/// worth keeping; writing straight to the final name would instead leave a
/// truncated file where a good one used to be.
fn move_into_place(
    dest: &mut dyn Vfs,
    partial: &Path,
    to: &Path,
    target_exists: bool,
) -> Result<(), VfsError> {
    if !target_exists || dest.capabilities().atomic_overwrite_rename {
        return dest.rename(partial, to);
    }

    let displaced = displaced_path(to);

    // Clear any leftover from an earlier interrupted swap, or the move aside
    // would fail for the same reason the overwrite does.
    let _ = dest.remove_file(&displaced);

    dest.rename(to, &displaced)?;

    match dest.rename(partial, to) {
        Ok(()) => {
            // The new file is in place; the displaced copy is now redundant.
            // A failure to remove it is not worth failing the transfer over --
            // the result is correct, just untidy -- but it is worth a log line,
            // since the leftover would appear in the next plan.
            if let Err(e) = dest.remove_file(&displaced) {
                tracing::warn!(
                    "{} was replaced, but the displaced copy at {} could not be removed: {e}",
                    to.display(),
                    displaced.display()
                );
            }
            Ok(())
        }
        Err(e) => {
            // Put the original back. If this also fails the file is still
            // there under the displaced name, which the error names.
            if let Err(rollback) = dest.rename(&displaced, to) {
                return Err(VfsError::Protocol {
                    context: format!(
                        "could not restore {} after a failed overwrite; the previous contents \
                         are at {}",
                        to.display(),
                        displaced.display()
                    ),
                    message: format!("{e} (and the rollback failed: {rollback})"),
                });
            }
            Err(e)
        }
    }
}

/// Where the previous contents go during a non-atomic swap.
fn displaced_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(".lazysync-replacing");
    path.with_file_name(name)
}

/// Remove one entry, or move it aside.
fn delete(dest: &mut dyn Vfs, dest_root: &Path, row: &Row, policy: &DeletePolicy) -> Outcome {
    let target = dest_root.join(&row.path);

    // Re-check: if it already matches what the source has, or is simply gone,
    // deleting it on the strength of an old plan is wrong.
    match dest.stat(&target) {
        Ok(now) => {
            if let Some(planned) = row.dest {
                if super::plan::Side::from_entry(&now) != planned {
                    return Outcome::SkippedStale {
                        why: "it changed after the plan was built".into(),
                    };
                }
            }
        }
        Err(e) if e.is_not_found() => {
            return Outcome::SkippedStale {
                why: "it is already gone".into(),
            }
        }
        Err(e) => return Outcome::Failed { why: e.to_string() },
    }

    match policy {
        DeletePolicy::Backup { dir } => {
            let backup = dest_root.join(dir).join(&row.path);

            if let Some(parent) = backup.parent() {
                if let Err(e) = dest.create_dir(parent) {
                    return Outcome::Failed {
                        why: format!("creating the backup directory: {e}"),
                    };
                }
            }

            match dest.rename(&target, &backup) {
                Ok(()) => Outcome::MovedToBackup { to: backup },
                // Not downgraded to a real delete. The user configured a
                // backup directory, and silently deleting instead because the
                // move failed would break exactly the promise it exists for.
                Err(e) => Outcome::Failed {
                    why: format!("moving it to the backup directory: {e}"),
                },
            }
        }

        DeletePolicy::Remove => {
            let result = if matches!(row.dest.map(|_| ()), Some(())) && is_dir_row(row) {
                dest.remove_dir_all(&target)
            } else {
                dest.remove_file(&target)
            };

            match result {
                Ok(()) => Outcome::Deleted,
                Err(e) => Outcome::Failed { why: e.to_string() },
            }
        }
    }
}

/// Whether a delete row refers to a directory.
///
/// Deletions are planned from a destination listing, so the row's own action
/// does not record the kind; a zero size with no timestamp is the signature a
/// directory entry leaves.
fn is_dir_row(row: &Row) -> bool {
    row.dest
        .is_some_and(|side| side.size == 0 && side.mtime.is_none())
}

/// `path` with the partial-transfer suffix appended.
///
/// Appended to the whole file name rather than replacing the extension, so
/// `notes.md` becomes `notes.md.lazysync-part` and cannot collide with a real
/// `notes.lazysync-part`.
fn partial_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use filetime::FileTime;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::Config,
        sync::{
            diff::{self, Direction},
            safety,
        },
        vfs::LocalFs,
    };

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

    fn build(t: &Trees, mirror: bool) -> Plan {
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        diff::plan(
            diff::Endpoint {
                fs: &mut source,
                root: &t.left,
                label: "local".into(),
            },
            diff::Endpoint {
                fs: &mut dest,
                root: &t.right,
                label: "remote".into(),
            },
            Direction {
                source: 0,
                delete_extraneous: mirror,
            },
            &Config::default(),
        )
    }

    fn run(t: &Trees, plan: &Plan, policy: &DeletePolicy) -> Report {
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        apply(
            &mut source,
            &t.left,
            &mut dest,
            &t.right,
            plan,
            policy,
            ".lazysync-part",
        )
    }

    #[test]
    fn a_create_copies_the_file_with_its_contents_and_timestamp() {
        let t = trees();
        write(&t.left.join("a.txt"), "hello", 1_700_000_000);

        let plan = build(&t, false);
        let report = run(&t, &plan, &DeletePolicy::Remove);

        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);
        assert_eq!(fs::read_to_string(t.right.join("a.txt")).unwrap(), "hello");

        // The timestamp has to come across, or the next sync sees a change.
        let mtime = filetime::FileTime::from_last_modification_time(
            &fs::metadata(t.right.join("a.txt")).unwrap(),
        );
        assert_eq!(mtime.unix_seconds(), 1_700_000_000);
    }

    #[test]
    fn nested_directories_are_created_before_their_contents() {
        let t = trees();
        write(&t.left.join("src/api/handler.rs"), "fn main() {}", 1000);

        let report = run(&t, &build(&t, false), &DeletePolicy::Remove);

        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);
        assert_eq!(
            fs::read_to_string(t.right.join("src/api/handler.rs")).unwrap(),
            "fn main() {}"
        );
    }

    #[test]
    fn an_overwrite_replaces_the_destination() {
        let t = trees();
        write(&t.left.join("a"), "new content", 2000);
        write(&t.right.join("a"), "old", 1000);

        let report = run(&t, &build(&t, false), &DeletePolicy::Remove);

        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);
        assert_eq!(
            fs::read_to_string(t.right.join("a")).unwrap(),
            "new content"
        );
    }

    #[test]
    fn a_skipped_row_is_not_touched() {
        let t = trees();
        write(&t.left.join("a"), "same", 1000);
        write(&t.right.join("a"), "same", 1000);

        let plan = build(&t, false);
        let report = run(&t, &plan, &DeletePolicy::Remove);

        assert!(
            report.applied.is_empty(),
            "nothing to do: {:?}",
            report.applied
        );
    }

    #[test]
    fn excluded_rows_are_not_applied() {
        let t = trees();
        write(&t.left.join("wanted"), "a", 1000);
        write(&t.left.join("unwanted"), "b", 1000);

        let mut plan = build(&t, false);
        let index = plan
            .rows
            .iter()
            .position(|r| r.path == Path::new("unwanted"))
            .unwrap();
        plan.toggle(index);

        run(&t, &plan, &DeletePolicy::Remove);

        assert!(t.right.join("wanted").exists());
        assert!(
            !t.right.join("unwanted").exists(),
            "a row excluded in the window must not be applied"
        );
    }

    /// The default policy: a deletion is recoverable.
    #[test]
    fn a_mirror_moves_deletions_to_the_backup_directory() {
        let t = trees();
        write(&t.left.join("keep"), "x", 1000);
        write(&t.right.join("keep"), "x", 1000);
        write(&t.right.join("extra"), "precious", 1000);

        let mut plan = build(&t, true);
        safety::check(&mut plan, &crate::config::Safety::default());

        let policy = DeletePolicy::Backup {
            dir: ".lazysync-backup".into(),
        };
        let report = run(&t, &plan, &policy);

        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);
        assert!(!t.right.join("extra").exists());
        assert_eq!(
            fs::read_to_string(t.right.join(".lazysync-backup/extra")).unwrap(),
            "precious",
            "a deletion under the default policy has to be recoverable"
        );
    }

    #[test]
    fn a_mirror_can_delete_permanently_when_asked() {
        let t = trees();
        write(&t.left.join("keep"), "x", 1000);
        write(&t.right.join("keep"), "x", 1000);
        write(&t.right.join("extra"), "y", 1000);

        let mut plan = build(&t, true);
        safety::check(&mut plan, &crate::config::Safety::default());
        run(&t, &plan, &DeletePolicy::Remove);

        assert!(!t.right.join("extra").exists());
        assert!(!t.right.join(".lazysync-backup").exists());
    }

    /// The whole reason for the partial-file dance.
    #[test]
    fn no_partial_files_are_left_behind_after_a_successful_run() {
        let t = trees();
        write(&t.left.join("a"), "hello", 1000);

        run(&t, &build(&t, false), &DeletePolicy::Remove);

        let leftovers: Vec<_> = fs::read_dir(&t.right)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("lazysync-part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// A plan is a snapshot. Applying it against a destination that moved on
    /// must skip rather than clobber something the user never reviewed.
    #[test]
    fn a_destination_that_changed_after_planning_is_skipped() {
        let t = trees();
        write(&t.left.join("a"), "source version", 2000);
        write(&t.right.join("a"), "planned against this", 1000);

        let plan = build(&t, false);

        // Someone else writes to the destination in the meantime.
        write(&t.right.join("a"), "SOMEONE ELSE'S WORK", 3000);

        let report = run(&t, &plan, &DeletePolicy::Remove);

        assert_eq!(report.skipped().count(), 1, "{:?}", report.applied);
        assert_eq!(
            fs::read_to_string(t.right.join("a")).unwrap(),
            "SOMEONE ELSE'S WORK",
            "it must not overwrite on the strength of a stale plan"
        );
    }

    #[test]
    fn a_deletion_whose_target_changed_is_skipped() {
        let t = trees();
        write(&t.left.join("keep"), "x", 1000);
        write(&t.right.join("keep"), "x", 1000);
        write(&t.right.join("extra"), "planned", 1000);

        let mut plan = build(&t, true);
        safety::check(&mut plan, &crate::config::Safety::default());

        write(&t.right.join("extra"), "changed since", 3000);

        let report = run(&t, &plan, &DeletePolicy::Remove);

        assert_eq!(report.skipped().count(), 1, "{:?}", report.applied);
        assert!(
            t.right.join("extra").exists(),
            "a file that changed after planning must not be deleted"
        );
    }

    #[test]
    fn a_deletion_that_already_happened_is_not_an_error() {
        let t = trees();
        write(&t.left.join("keep"), "x", 1000);
        write(&t.right.join("keep"), "x", 1000);
        write(&t.right.join("extra"), "y", 1000);

        let mut plan = build(&t, true);
        safety::check(&mut plan, &crate::config::Safety::default());

        fs::remove_file(t.right.join("extra")).unwrap();

        let report = run(&t, &plan, &DeletePolicy::Remove);
        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);
        assert_eq!(report.skipped().count(), 1);
    }

    /// A refused plan must do nothing at all, not partially apply.
    #[test]
    fn a_refused_plan_applies_nothing() {
        let t = trees();
        for i in 0..10 {
            write(&t.right.join(format!("f{i}")), "x", 1000);
        }

        let mut plan = build(&t, true);
        safety::check(&mut plan, &crate::config::Safety::default());
        assert!(
            plan.refusal.is_some(),
            "a full-destination mirror should refuse"
        );

        let report = run(&t, &plan, &DeletePolicy::Remove);

        assert!(report.applied.is_empty());
        assert!(
            t.right.join("f0").exists(),
            "a refused plan must not have deleted anything"
        );
    }

    /// One failure must not abandon the rest: a sync whose shape you know
    /// beats one that stopped somewhere unspecified.
    #[cfg(unix)]
    #[test]
    fn one_failure_does_not_abandon_the_other_rows() {
        use std::os::unix::fs::PermissionsExt;

        let t = trees();
        write(&t.left.join("readable"), "a", 1000);
        write(&t.left.join("locked"), "b", 1000);
        fs::set_permissions(t.left.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();

        let report = run(&t, &build(&t, false), &DeletePolicy::Remove);
        fs::set_permissions(t.left.join("locked"), fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(report.failures().count(), 1, "{:?}", report.applied);
        assert!(
            t.right.join("readable").exists(),
            "the readable file should still have been copied"
        );
        assert!(report.summary().contains("failed"), "{}", report.summary());
    }

    #[test]
    fn the_report_summarises_what_happened() {
        let t = trees();
        write(&t.left.join("a"), "hello", 1000);

        let report = run(&t, &build(&t, false), &DeletePolicy::Remove);

        assert_eq!(report.succeeded(), 1);
        let summary = report.summary();
        assert!(summary.contains("1 applied"), "{summary}");
    }

    /// A backend whose rename refuses an existing target, like SFTP's.
    ///
    /// Wraps `LocalFs` and reports `atomic_overwrite_rename: false`, failing
    /// any rename onto a path that exists -- which is exactly what the
    /// OpenSSH server does. Without this, the swap path would only ever be
    /// exercised by the SFTP integration tests, which do not run everywhere.
    struct NonAtomicRename(LocalFs);

    impl Vfs for NonAtomicRename {
        fn scheme(&self) -> &str {
            "pretend-sftp"
        }
        fn display_path(&self, path: &Path) -> String {
            self.0.display_path(path)
        }
        fn capabilities(&self) -> crate::vfs::Capabilities {
            crate::vfs::Capabilities {
                atomic_overwrite_rename: false,
                ..self.0.capabilities()
            }
        }
        fn list(&mut self, path: &Path) -> Result<Vec<crate::vfs::Entry>, crate::vfs::VfsError> {
            self.0.list(path)
        }
        fn is_dir(&mut self, path: &Path) -> bool {
            self.0.is_dir(path)
        }
        fn create_dir(&mut self, path: &Path) -> Result<(), crate::vfs::VfsError> {
            self.0.create_dir(path)
        }
        fn stat(&mut self, path: &Path) -> Result<crate::vfs::Entry, crate::vfs::VfsError> {
            self.0.stat(path)
        }
        fn read_into(
            &mut self,
            path: &Path,
            dest: &mut (dyn std::io::Write + Send),
        ) -> Result<u64, crate::vfs::VfsError> {
            self.0.read_into(path, dest)
        }
        fn write_from(
            &mut self,
            path: &Path,
            metadata: &WriteMetadata,
            src: &mut (dyn std::io::Read + Send),
        ) -> Result<u64, crate::vfs::VfsError> {
            self.0.write_from(path, metadata, src)
        }
        fn set_times(
            &mut self,
            path: &Path,
            metadata: &WriteMetadata,
        ) -> Result<(), crate::vfs::VfsError> {
            self.0.set_times(path, metadata)
        }
        fn remove_file(&mut self, path: &Path) -> Result<(), crate::vfs::VfsError> {
            self.0.remove_file(path)
        }
        fn remove_dir_all(&mut self, path: &Path) -> Result<(), crate::vfs::VfsError> {
            self.0.remove_dir_all(path)
        }
        fn rename(&mut self, from: &Path, to: &Path) -> Result<(), crate::vfs::VfsError> {
            if to.exists() {
                return Err(crate::vfs::VfsError::PermissionDenied(format!(
                    "{} already exists",
                    to.display()
                )));
            }
            self.0.rename(from, to)
        }
        fn home(&mut self) -> Option<PathBuf> {
            self.0.home()
        }
        fn root(&self) -> PathBuf {
            self.0.root()
        }
    }

    /// The bug the SFTP integration test caught: a rename that will not
    /// replace an existing file breaks every overwrite.
    #[test]
    fn an_overwrite_succeeds_on_a_backend_whose_rename_cannot_replace() {
        let t = trees();
        write(&t.left.join("a"), "new content", 2000);
        write(&t.right.join("a"), "old", 1000);

        let plan = build(&t, false);

        let mut source = LocalFs::new();
        let mut dest = NonAtomicRename(LocalFs::new());
        let report = apply(
            &mut source,
            &t.left,
            &mut dest,
            &t.right,
            &plan,
            &DeletePolicy::Remove,
            ".lazysync-part",
        );

        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);
        assert_eq!(
            fs::read_to_string(t.right.join("a")).unwrap(),
            "new content"
        );
    }

    /// And the swap must not leave its own debris: a displaced copy left
    /// behind would show up in the next plan as a file to sync back.
    #[test]
    fn the_swap_leaves_neither_a_partial_nor_a_displaced_copy() {
        let t = trees();
        write(&t.left.join("a"), "new content", 2000);
        write(&t.right.join("a"), "old", 1000);

        let plan = build(&t, false);
        let mut source = LocalFs::new();
        let mut dest = NonAtomicRename(LocalFs::new());
        apply(
            &mut source,
            &t.left,
            &mut dest,
            &t.right,
            &plan,
            &DeletePolicy::Remove,
            ".lazysync-part",
        );

        let names: Vec<String> = fs::read_dir(&t.right)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a".to_string()], "{names:?}");
    }

    /// A leftover from an earlier interrupted swap must not block the next
    /// one, or a single failure would wedge that file forever.
    #[test]
    fn a_leftover_displaced_copy_does_not_block_a_later_overwrite() {
        let t = trees();
        write(&t.left.join("a"), "new content", 2000);
        write(&t.right.join("a"), "old", 1000);
        // What an interrupted swap would have left.
        write(&t.right.join("a.lazysync-replacing"), "debris", 500);

        let plan = build(&t, false);
        let mut source = LocalFs::new();
        let mut dest = NonAtomicRename(LocalFs::new());
        let report = apply(
            &mut source,
            &t.left,
            &mut dest,
            &t.right,
            &plan,
            &DeletePolicy::Remove,
            ".lazysync-part",
        );

        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);
        assert_eq!(
            fs::read_to_string(t.right.join("a")).unwrap(),
            "new content"
        );
        assert!(!t.right.join("a.lazysync-replacing").exists());
    }

    #[test]
    fn the_partial_suffix_is_appended_to_the_whole_name() {
        assert_eq!(
            partial_path(Path::new("/srv/notes.md"), ".lazysync-part"),
            PathBuf::from("/srv/notes.md.lazysync-part"),
        );
        assert_eq!(
            partial_path(Path::new("/srv/archive.tar.gz"), ".part"),
            PathBuf::from("/srv/archive.tar.gz.part"),
        );
    }

    #[test]
    fn an_empty_file_transfers() {
        let t = trees();
        write(&t.left.join("empty"), "", 1000);

        let report = run(&t, &build(&t, false), &DeletePolicy::Remove);

        assert_eq!(report.failures().count(), 0, "{:?}", report.applied);
        assert!(t.right.join("empty").exists());
        assert_eq!(fs::metadata(t.right.join("empty")).unwrap().len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn the_executable_bit_is_preserved() {
        use std::os::unix::fs::PermissionsExt;

        let t = trees();
        write(&t.left.join("run.sh"), "#!/bin/sh\n", 1000);
        fs::set_permissions(t.left.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();

        run(&t, &build(&t, false), &DeletePolicy::Remove);

        let mode = fs::metadata(t.right.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "a script that arrives non-executable is broken"
        );
    }
}
