//! The rails that stop a mistyped path from emptying a directory.
//!
//! A sync tool's worst outcome is not failing: it is succeeding at something
//! you did not mean. Pointing a mirror at the wrong root is a two-character
//! typo away, and the plan that follows looks perfectly reasonable -- "delete
//! 4,812 files" is exactly what a mirror of an empty directory *should* say.
//!
//! So these checks run on the plan, before the confirmation window draws, and
//! a refusal is recorded in [`Plan::refusal`]. The window then shows the plan
//! *and* the reason it will not apply, which is more use than an error instead
//! of a plan: you can see what it was about to do.

use crate::{
    config::Safety as SafetyConfig,
    sync::plan::{Action, Plan, Row},
};

/// Why a plan was refused.
///
/// No `Eq`: the fraction is an `f64`, and a safety limit is not something to
/// compare for exact bit equality anyway.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// Deleting more than the configured fraction of the destination.
    TooMuchDeleted {
        deletes: usize,
        total: usize,
        fraction: f64,
        limit: f64,
    },
    /// Deleting more than the configured absolute number of files.
    TooManyDeleted { deletes: usize, limit: usize },
    /// Deleting *everything* the plan knows about on the destination.
    ///
    /// Its own variant because it is the signature of the mistake this module
    /// exists for -- a mirror pointed at the wrong root, or at an empty one --
    /// and because it has to be caught at any tree size, including the small
    /// ones where a percentage says nothing.
    WouldEmptyDestination { deletes: usize },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooMuchDeleted {
                deletes,
                total,
                fraction,
                limit,
            } => write!(
                f,
                "REFUSED: this would delete {deletes} of {total} files on the destination \
                 ({:.0}%, limit {:.0}%). If the sync root is right, raise \
                 safety.max_delete_fraction",
                fraction * 100.0,
                limit * 100.0
            ),
            Self::TooManyDeleted { deletes, limit } => write!(
                f,
                "REFUSED: this would delete {deletes} files, over the \
                 safety.max_delete_files limit of {limit}"
            ),
            Self::WouldEmptyDestination { deletes } => write!(
                f,
                "REFUSED: this would delete every one of the {deletes} files on the \
                 destination and leave nothing. Check the sync root; if this really is what \
                 you want, exclude a row or set safety.max_delete_fraction = 0"
            ),
        }
    }
}

/// Below this many destination files, the fraction limit is not applied.
///
/// A percentage of a tiny tree is noise: removing one extraneous file from a
/// directory of two is "50% deleted", which would refuse a completely
/// reasonable mirror. rsync's `--max-delete` is a count for the same reason.
///
/// The dangerous case at small sizes is not "a large share" but "all of it",
/// and [`Refusal::WouldEmptyDestination`] catches that at any size -- so
/// nothing is given up by having this floor.
const FRACTION_FLOOR: usize = 10;

/// Check a plan against the safety limits, recording any refusal on it.
///
/// Called after planning and after every inclusion change, because excluding
/// rows can bring a plan back under the limit -- and a refusal that does not
/// clear when you fix it is a dead end.
pub fn check(plan: &mut Plan, config: &SafetyConfig) {
    plan.refusal = if plan.bidirectional {
        // Each side gets its own verdict. Pooling them would let a large tree
        // mask a small one: deleting five of ten files locally alongside five
        // of a thousand remotely reads as 1% of the total while halving the
        // local tree. The fraction is only meaningful against the tree it is
        // a fraction *of*.
        [0usize, 1]
            .into_iter()
            .find_map(|pane| refuse(plan, config, Some(pane)))
            .map(|r| r.to_string())
    } else {
        refuse(plan, config, None).map(|r| r.to_string())
    };
}

/// Decide whether to refuse, optionally restricted to one side.
///
/// `pane` is `None` for a one-directional plan, where every row writes to the
/// same destination and there is only one tree to measure against.
fn refuse(plan: &Plan, config: &SafetyConfig, pane: Option<usize>) -> Option<Refusal> {
    let on_side = |row: &Row| pane.is_none_or(|p| row.target_pane == p);

    let deletes = plan.destructive_rows().filter(|r| on_side(r)).count();
    if deletes == 0 {
        return None;
    }

    if config.max_delete_files > 0 && deletes > config.max_delete_files {
        return Some(Refusal::TooManyDeleted {
            deletes,
            limit: config.max_delete_files,
        });
    }

    if config.max_delete_fraction > 0.0 {
        // The denominator is every file the destination has that the plan knows
        // about: the ones being deleted plus the ones being kept. Using only
        // the rows marked for deletion would make the fraction always 1.
        let total = plan
            .rows
            .iter()
            .filter(|row| {
                on_side(row)
                    && row.side_of(row.target_pane).is_some()
                    && !matches!(row.action, Action::CreateDir)
            })
            .count();

        if total == 0 {
            return None;
        }

        // Emptying the destination entirely is refused whatever the size,
        // because that is what a mirror of the wrong root looks like.
        if deletes == total {
            return Some(Refusal::WouldEmptyDestination { deletes });
        }

        // Below the floor a percentage is meaningless; the check above is the
        // one that matters at those sizes.
        if total < FRACTION_FLOOR {
            return None;
        }

        let fraction = deletes as f64 / total as f64;
        if fraction > config.max_delete_fraction {
            return Some(Refusal::TooMuchDeleted {
                deletes,
                total,
                fraction,
                limit: config.max_delete_fraction,
            });
        }
    }

    None
}

/// Where a deleted file goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletePolicy {
    /// Move it under a backup directory inside the destination root, keeping
    /// its relative path. Recoverable, which is why it is the default.
    Backup { dir: String },
    /// Actually delete it.
    Remove,
}

impl DeletePolicy {
    pub fn from_config(config: &SafetyConfig) -> Self {
        if config.backup_dir.trim().is_empty() {
            Self::Remove
        } else {
            Self::Backup {
                dir: config.backup_dir.trim().to_owned(),
            }
        }
    }

    /// A one-line description for the confirmation window's header.
    ///
    /// The window says this out loud because "delete" meaning "move aside" and
    /// "delete" meaning "gone" are very different things to be agreeing to.
    pub fn describe(&self) -> String {
        match self {
            Self::Backup { dir } => format!("deletions move to {dir}/"),
            Self::Remove => "deletions are permanent".into(),
        }
    }
}

/// Whether a plan's backup directory would itself be synced.
///
/// A backup directory inside the destination root is in the tree the next plan
/// walks. Left alone, a mirror would then see it as extraneous on the source
/// side and delete it -- throwing away the backups that exist to be a safety
/// net. The planner excludes it by name.
pub fn is_backup_path(policy: &DeletePolicy, relative: &std::path::Path) -> bool {
    match policy {
        DeletePolicy::Backup { dir } => relative
            .components()
            .next()
            .is_some_and(|first| first.as_os_str() == dir.as_str()),
        DeletePolicy::Remove => false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{
        sync::{
            compare::Method,
            plan::{self, Row},
        },
        vfs::{Entry, EntryKind},
    };

    fn entry(size: u64) -> Entry {
        Entry {
            name: "f".into(),
            kind: EntryKind::File,
            size,
            mtime: None,
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }

    fn deletes(count: usize) -> Vec<Row> {
        (0..count)
            .map(|i| {
                plan::delete_row(
                    PathBuf::from(format!("d{i}")),
                    &entry(1),
                    Method::SizeMtime,
                    1,
                )
            })
            .collect()
    }

    fn keeps(count: usize) -> Vec<Row> {
        (0..count)
            .map(|i| {
                plan::compare_row(
                    PathBuf::from(format!("k{i}")),
                    &entry(1),
                    &entry(1),
                    crate::sync::compare::Reason::SameSizeAndTime,
                    Method::SizeMtime,
                    1,
                )
            })
            .collect()
    }

    fn plan_of(rows: Vec<Row>) -> Plan {
        Plan {
            rows,
            ..Plan::default()
        }
    }

    fn config(fraction: f64, files: usize) -> SafetyConfig {
        SafetyConfig {
            backup_dir: ".lazysync-backup".into(),
            max_delete_fraction: fraction,
            max_delete_files: files,
            confirm_outside_root: true,
            conflict_suffix: ".lazysync-conflict".into(),
        }
    }

    /// Rows targeting a named pane, for the two-sided checks.
    fn deletes_on(pane: usize, count: usize) -> Vec<Row> {
        (0..count)
            .map(|i| {
                plan::delete_row(
                    PathBuf::from(format!("d{pane}-{i}")),
                    &entry(1),
                    Method::SizeMtime,
                    pane,
                )
            })
            .collect()
    }

    fn keeps_on(pane: usize, count: usize) -> Vec<Row> {
        (0..count)
            .map(|i| {
                plan::compare_row(
                    PathBuf::from(format!("k{pane}-{i}")),
                    &entry(1),
                    &entry(1),
                    crate::sync::compare::Reason::SameSizeAndTime,
                    Method::SizeMtime,
                    pane,
                )
            })
            .collect()
    }

    /// **A bidirectional plan is judged one side at a time.**
    ///
    /// Pooling the two would let a large tree hide the destruction of a small
    /// one: six deletions out of ten local files is 60% of that tree, but
    /// alongside a thousand untouched remote files it reads as well under 1%
    /// of the total and would sail past a 25% limit.
    #[test]
    fn a_bisync_measures_each_side_against_its_own_tree() {
        let mut rows = deletes_on(0, 6);
        rows.extend(keeps_on(0, 4));
        rows.extend(keeps_on(1, 1000));

        let mut pooled = plan_of(rows.clone());
        check(&mut pooled, &config(0.25, 0));
        assert!(
            pooled.refusal.is_none(),
            "a one-directional plan has only one tree, and 6 of 1010 is under the limit"
        );

        let mut both_ways = Plan {
            bidirectional: true,
            ..plan_of(rows)
        };
        check(&mut both_ways, &config(0.25, 0));
        assert!(
            both_ways.refusal.is_some(),
            "6 of the local side's 10 files is 60%, and that is what matters"
        );
    }

    /// And the converse: a bidirectional plan whose deletions are modest on
    /// both sides is still allowed. Checking per side must not become a
    /// refusal of every two-way sync that removes anything.
    #[test]
    fn a_bisync_deleting_a_little_on_each_side_is_allowed() {
        let mut rows = deletes_on(0, 1);
        rows.extend(keeps_on(0, 20));
        rows.extend(deletes_on(1, 2));
        rows.extend(keeps_on(1, 30));

        let mut plan = Plan {
            bidirectional: true,
            ..plan_of(rows)
        };
        check(&mut plan, &config(0.25, 0));
        assert!(
            plan.refusal.is_none(),
            "1 of 21 and 2 of 32 are both well under the limit: {:?}",
            plan.refusal
        );
    }

    #[test]
    fn a_plan_with_no_deletions_is_never_refused() {
        let mut plan = plan_of(keeps(100));
        check(&mut plan, &config(0.25, 0));
        assert!(plan.refusal.is_none());
    }

    #[test]
    fn a_small_fraction_of_deletions_is_allowed() {
        // 1 delete out of 10 known destination files: 10%, under the 25% limit.
        let mut rows = deletes(1);
        rows.extend(keeps(9));
        let mut plan = plan_of(rows);

        check(&mut plan, &config(0.25, 0));
        assert!(plan.refusal.is_none(), "{:?}", plan.refusal);
        assert!(plan.can_apply());
    }

    /// The mistyped-root case: a mirror against an empty source wants to
    /// delete everything, and that must not be a thing you can agree to by
    /// reflex.
    #[test]
    fn deleting_the_whole_destination_is_refused_at_any_size() {
        for count in [1, 2, 5, 50] {
            let mut plan = plan_of(deletes(count));
            check(&mut plan, &config(0.25, 0));

            let refusal = plan
                .refusal
                .as_ref()
                .unwrap_or_else(|| panic!("wiping {count} of {count} files should refuse"));
            assert!(refusal.contains("REFUSED"), "{refusal}");
            assert!(refusal.contains("leave nothing"), "{refusal}");
            assert!(!plan.can_apply());
        }
    }

    /// Over the floor, the percentage governs.
    #[test]
    fn deleting_most_of_a_large_destination_is_refused() {
        // 8 deletes out of 20: 40%, over the 25% limit.
        let mut rows = deletes(8);
        rows.extend(keeps(12));
        let mut plan = plan_of(rows);

        check(&mut plan, &config(0.25, 0));

        let refusal = plan.refusal.as_ref().expect("40% is over 25%");
        assert!(refusal.contains("8 of 20"), "{refusal}");
        assert!(
            refusal.contains("max_delete_fraction"),
            "it has to say how to proceed deliberately: {refusal}"
        );
    }

    /// A percentage of a tiny tree is noise. Removing one extraneous file
    /// from a directory of two is "50% deleted", and refusing that would make
    /// the guard fire constantly on exactly the small syncs where it is least
    /// needed.
    #[test]
    fn a_small_tree_is_not_judged_by_percentage() {
        let mut rows = deletes(1);
        rows.extend(keeps(1));
        let mut plan = plan_of(rows);

        check(&mut plan, &config(0.25, 0));

        assert!(
            plan.refusal.is_none(),
            "1 of 2 files is 50%, but refusing it would be useless pedantry: {:?}",
            plan.refusal
        );
        assert!(plan.can_apply());
    }

    /// The absolute count still applies below the floor, for anyone who wants
    /// a hard cap on small trees too.
    #[test]
    fn the_file_limit_still_applies_below_the_fraction_floor() {
        let mut rows = deletes(3);
        rows.extend(keeps(2));
        let mut plan = plan_of(rows);

        check(&mut plan, &config(0.25, 2));
        assert!(
            plan.refusal
                .as_ref()
                .is_some_and(|r| r.contains("limit of 2")),
            "{:?}",
            plan.refusal
        );
    }

    #[test]
    fn the_absolute_file_limit_is_enforced_when_set() {
        let mut rows = deletes(11);
        rows.extend(keeps(1000));
        let mut plan = plan_of(rows);

        check(&mut plan, &config(0.0, 10));
        let refusal = plan.refusal.as_ref().expect("over the file limit");
        assert!(refusal.contains("11 files"), "{refusal}");
        assert!(refusal.contains("limit of 10"), "{refusal}");
    }

    #[test]
    fn a_zero_file_limit_means_no_limit() {
        let mut rows = deletes(1000);
        rows.extend(keeps(10_000));
        let mut plan = plan_of(rows);

        check(&mut plan, &config(0.0, 0));
        assert!(
            plan.refusal.is_none(),
            "both limits disabled means no refusal: {:?}",
            plan.refusal
        );
    }

    /// A refusal has to clear when the user excludes the offending rows, or
    /// the window becomes a dead end.
    #[test]
    fn excluding_deletions_clears_the_refusal() {
        let mut plan = plan_of(deletes(50));
        check(&mut plan, &config(0.25, 0));
        assert!(plan.refusal.is_some());

        plan.set_all(false);
        check(&mut plan, &config(0.25, 0));

        assert!(
            plan.refusal.is_none(),
            "excluding the deletions should make the plan acceptable again"
        );
    }

    /// Excluding *some* of them should also help, proportionally.
    #[test]
    fn excluding_enough_deletions_brings_a_plan_under_the_limit() {
        let mut rows = deletes(5);
        rows.extend(keeps(15));
        let mut plan = plan_of(rows);

        check(&mut plan, &config(0.20, 0));
        assert!(plan.refusal.is_some(), "5/20 is 25%, over the 20% limit");

        plan.toggle(0); // exclude one deletion: 4/20 = 20%, not over
        check(&mut plan, &config(0.20, 0));
        assert!(plan.refusal.is_none(), "{:?}", plan.refusal);
    }

    /// The floor must not become a hole: a plan that empties the destination
    /// is refused even when it is small enough to skip the percentage check.
    #[test]
    fn the_fraction_floor_does_not_let_a_full_wipe_through() {
        for count in 1..FRACTION_FLOOR {
            let mut plan = plan_of(deletes(count));
            check(&mut plan, &config(0.25, 0));
            assert!(
                plan.refusal.is_some(),
                "deleting all {count} destination files should still refuse"
            );
        }
    }

    #[test]
    fn the_default_policy_moves_deletions_aside_rather_than_removing_them() {
        let policy = DeletePolicy::from_config(&config(0.25, 0));
        assert!(matches!(policy, DeletePolicy::Backup { .. }));
        assert!(policy.describe().contains(".lazysync-backup"));
    }

    #[test]
    fn an_empty_backup_dir_means_real_deletion_and_says_so() {
        let mut c = config(0.25, 0);
        c.backup_dir = String::new();

        let policy = DeletePolicy::from_config(&c);
        assert_eq!(policy, DeletePolicy::Remove);
        assert!(
            policy.describe().contains("permanent"),
            "agreeing to 'delete' should not be ambiguous about what that means"
        );
    }

    #[test]
    fn whitespace_only_backup_dir_counts_as_unset() {
        let mut c = config(0.25, 0);
        c.backup_dir = "   ".into();
        assert_eq!(DeletePolicy::from_config(&c), DeletePolicy::Remove);
    }

    /// Without this, a mirror would delete its own backups on the next run.
    #[test]
    fn the_backup_directory_is_recognised_so_it_is_not_itself_synced() {
        let policy = DeletePolicy::from_config(&config(0.25, 0));

        assert!(is_backup_path(&policy, Path::new(".lazysync-backup")));
        assert!(is_backup_path(
            &policy,
            Path::new(".lazysync-backup/src/old.rs")
        ));

        assert!(!is_backup_path(&policy, Path::new("src/.lazysync-backup")));
        assert!(!is_backup_path(&policy, Path::new("src/main.rs")));
        assert!(!is_backup_path(
            &policy,
            Path::new(".lazysync-backup-but-not")
        ));
    }

    #[test]
    fn nothing_is_a_backup_path_when_deletions_are_permanent() {
        assert!(!is_backup_path(
            &DeletePolicy::Remove,
            Path::new(".lazysync-backup/x")
        ));
    }
}
