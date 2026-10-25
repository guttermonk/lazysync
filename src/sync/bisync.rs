//! Two-way sync: deciding what changed on which side.
//!
//! A one-directional sync needs only the two trees. Bisync needs a third
//! input -- what they looked like last time -- because without it the two
//! readings of "here on the left, absent on the right" are indistinguishable
//! and call for opposite actions: copy it right, or delete it on the left.
//! [`super::state::Snapshot`] supplies that third input.
//!
//! So every path is classified from three facts: was it in the snapshot, is it
//! on the left now, is it on the right now. The result is [`Verdict`], and the
//! table in [`classify`] is the whole of the logic. It is a pure function of
//! those three inputs, which is why it can be tested exhaustively without a
//! filesystem.
//!
//! **The first run cannot propagate deletions.** With no snapshot there is no
//! basis for calling anything a deletion -- every path that exists on one side
//! only might equally be new. rclone's bisync refuses outright and makes you
//! pass `--resync`; lazysync instead plans the first run as a union (copy
//! everything each side is missing, conflict where both have different
//! content) and *never* deletes. That is strictly safer than refusing, because
//! refusing teaches people to pass the override without reading it.
//!
//! **Nothing here picks the newer file.** When both sides changed, the
//! temptation is to let the later timestamp win. That is wrong twice over: it
//! silently discards the other side's edit, and "later" is a claim about two
//! clocks that may differ by more than the edits do. Such paths become
//! conflicts the user resolves, which is the whole reason
//! [`super::plan::Resolution`] exists.

use std::path::Path;

use chrono::{DateTime, Utc};

use super::{
    compare::{Comparator, Reason},
    diff::{Endpoint, MAX_DEPTH},
    filters::Filters,
    plan::{self, Plan, Row},
    state::{Record, Snapshot},
};
use crate::{
    config::Config,
    vfs::{Capabilities, Entry, EntryKind, Vfs, VfsError},
};

/// What should happen to one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to do, and nothing to record that is not recorded already.
    InAgreement,
    /// Copy from `source_pane` to the other side.
    Copy { source_pane: usize, reason: Reason },
    /// Delete from `target_pane`, because it was removed on the other side.
    Propagate { target_pane: usize },
    /// Both sides have claims. The user decides.
    Conflict { reason: Reason },
    /// Gone from both sides. Only the snapshot needs updating.
    GoneFromBoth,
}

/// One side's view of a path, as the classifier needs it.
///
/// A pared-down [`Entry`]: size and time are all that the comparison uses, and
/// taking the whole entry would invite the classifier to look at fields the
/// snapshot cannot store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observed {
    pub size: u64,
    pub mtime: Option<DateTime<Utc>>,
}

impl Observed {
    pub fn from_entry(entry: &Entry) -> Self {
        Self {
            size: entry.size,
            mtime: entry.mtime,
        }
    }
}

/// Decide what to do about one path.
///
/// `local` and `remote` are what each side has now; `was` is what the snapshot
/// recorded, or `None` if the path was not in it. `first_run` suppresses every
/// deletion, because with no snapshot there is no evidence that anything was
/// deleted rather than created.
///
/// The `tolerance` is the comparator's, so "changed since last sync" is judged
/// by the same rule as "differs between the sides". Using a tighter rule here
/// would make a file count as changed on both sides -- a conflict -- purely
/// because the destination rounded its timestamp.
pub fn classify(
    local: Option<Observed>,
    remote: Option<Observed>,
    was: Option<&Record>,
    comparator: &Comparator,
    first_run: bool,
) -> Verdict {
    let sides_agree = match (local, remote) {
        (Some(l), Some(r)) => same(l, r, comparator),
        _ => false,
    };

    match (local, remote, was) {
        // ── Neither side has it ──────────────────────────────────────────
        (None, None, _) => Verdict::GoneFromBoth,

        // ── No history for this path ─────────────────────────────────────
        //
        // Either it is genuinely new, or this is a first run and everything
        // looks new. Both readings agree on what to do when only one side has
        // it: copy it. When both have it, the contents decide.
        (Some(_), None, None) => Verdict::Copy {
            source_pane: 0,
            reason: Reason::NewOnOneSide,
        },
        (None, Some(_), None) => Verdict::Copy {
            source_pane: 1,
            reason: Reason::NewOnOneSide,
        },
        (Some(_), Some(_), None) => {
            if sides_agree {
                Verdict::InAgreement
            } else {
                // Two files of the same name appeared independently with
                // different contents, and there is no common ancestor to
                // prefer either against.
                Verdict::Conflict {
                    reason: Reason::BothNew,
                }
            }
        }

        // ── Present on both, with history ────────────────────────────────
        (Some(l), Some(r), Some(was)) => {
            let local_changed = !matches(l, was.side(0), comparator);
            let remote_changed = !matches(r, was.side(1), comparator);

            match (local_changed, remote_changed) {
                (false, false) => Verdict::InAgreement,
                (true, false) => Verdict::Copy {
                    source_pane: 0,
                    reason: Reason::ChangedOnOneSide,
                },
                (false, true) => Verdict::Copy {
                    source_pane: 1,
                    reason: Reason::ChangedOnOneSide,
                },
                (true, true) if sides_agree => {
                    // Both edited, and they arrived at the same thing -- two
                    // machines running the same build, usually. Converged on
                    // their own; presenting this as a conflict would be noise.
                    Verdict::InAgreement
                }
                (true, true) => Verdict::Conflict {
                    reason: Reason::BothChanged,
                },
            }
        }

        // ── Gone from one side, with history ─────────────────────────────
        //
        // This is where the snapshot earns its existence: the path was there
        // last time and is now missing on one side, so it was deleted there.
        (None, Some(r), Some(was)) => {
            deletion(1, r, was, comparator, first_run, Reason::NewOnOneSide)
        }
        (Some(l), None, Some(was)) => {
            deletion(0, l, was, comparator, first_run, Reason::NewOnOneSide)
        }
    }
}

/// One side deleted the path. Decide whether that propagates.
///
/// `surviving_pane` is the side that still has it. If that side *also* changed
/// the file since the last sync, there are two incompatible intentions --
/// somebody removed it and somebody else edited it -- and propagating either
/// one silently discards the other. That is a conflict, and it is the case
/// most easily got wrong.
fn deletion(
    surviving_pane: usize,
    surviving: Observed,
    was: &Record,
    comparator: &Comparator,
    first_run: bool,
    recreate_reason: Reason,
) -> Verdict {
    // With no snapshot, "deleted" is not a conclusion that can be drawn: the
    // path is simply on one side only, so it is copied. This is why a first
    // run can never destroy anything.
    if first_run {
        return Verdict::Copy {
            source_pane: surviving_pane,
            reason: recreate_reason,
        };
    }

    if matches(surviving, was.side(surviving_pane), comparator) {
        // Untouched on the surviving side, so the deletion is the only
        // intention expressed and it propagates.
        Verdict::Propagate {
            target_pane: surviving_pane,
        }
    } else {
        Verdict::Conflict {
            reason: Reason::DeletedVersusChanged,
        }
    }
}

/// Whether what a side has now is what the snapshot recorded for it.
fn matches(now: Observed, then: (u64, Option<DateTime<Utc>>), comparator: &Comparator) -> bool {
    let (size, mtime) = then;
    if now.size != size {
        return false;
    }

    // Size matched. If either timestamp is missing there is nothing further to
    // check, and treating that as a change would make every run after a sync
    // to a timestamp-less backend look like a fresh edit.
    match (now.mtime, mtime) {
        (Some(a), Some(b)) => (a.timestamp() - b.timestamp()).abs() <= comparator.tolerance_secs(),
        _ => true,
    }
}

/// Whether the two sides hold the same file, by the comparator's rule.
fn same(local: Observed, remote: Observed, comparator: &Comparator) -> bool {
    if local.size != remote.size {
        return false;
    }
    match (local.mtime, remote.mtime) {
        (Some(a), Some(b)) if comparator.has_timestamps() => {
            (a.timestamp() - b.timestamp()).abs() <= comparator.tolerance_secs()
        }
        // No usable timestamps: equal sizes is the whole of the available
        // evidence, which is the same judgement `compare` makes.
        _ => true,
    }
}

/// Record a path as being in agreement, for the next run's snapshot.
pub fn record(snapshot: &mut Snapshot, relative: &Path, local: &Entry, remote: &Entry) {
    snapshot.insert(relative, Record::from_entries(local, remote));
}

/// Build a two-way plan.
///
/// Symmetric where [`super::diff::plan`] is source-oriented: there is no
/// source side, so each directory is listed on both and the walk works over
/// the *union* of their names. A name present on one side only is as
/// interesting as one present on both, which is exactly what a source-driven
/// walk cannot express.
pub fn plan(
    local: Endpoint<'_>,
    remote: Endpoint<'_>,
    snapshot: &Snapshot,
    config: &Config,
    filters: &Filters,
) -> Plan {
    let Endpoint {
        fs: local,
        root: local_root,
        label: local_label,
    } = local;
    let Endpoint {
        fs: remote,
        root: remote_root,
        label: remote_label,
    } = remote;

    let comparator = Comparator::new(&config.compare, local.capabilities(), remote.capabilities());
    let policy = super::safety::DeletePolicy::from_config(&config.safety);
    let first_run = snapshot.is_first_run();

    let mut built = Plan {
        source_root: local_root.to_path_buf(),
        dest_root: remote_root.to_path_buf(),
        source_label: local_label,
        dest_label: remote_label,
        method: comparator.method(),
        tolerance_secs: comparator.tolerance_secs(),
        bidirectional: true,
        first_run,
        ..Plan::default()
    };

    // Caveats are per *direction* here, which a one-directional plan does not
    // have to think about: a push to FTP loses its timestamp and the pull back
    // does not. Computed once each.
    let caveats = Caveats {
        to_remote: caveat(local.capabilities(), remote.capabilities()),
        to_local: caveat(remote.capabilities(), local.capabilities()),
    };

    walk(
        &mut Sides {
            local,
            local_root,
            remote,
            remote_root,
        },
        Path::new(""),
        0,
        &comparator,
        snapshot,
        config,
        filters,
        &policy,
        &caveats,
        first_run,
        &mut built,
    );

    built.sort();
    built
}

/// The two backends and their roots, bundled so the recursive walk takes one
/// argument for them rather than four.
struct Sides<'a> {
    local: &'a mut dyn Vfs,
    local_root: &'a Path,
    remote: &'a mut dyn Vfs,
    remote_root: &'a Path,
}

impl Sides<'_> {
    /// List one side of a directory.
    ///
    /// A missing directory is an empty listing, not an error: in a two-way
    /// sync it is the ordinary case for a subtree that exists on one side
    /// only. A directory that exists but cannot be *read* is different and is
    /// reported, because planning against a listing we do not have would mean
    /// proposing deletions for files we simply could not see.
    fn list(&mut self, pane: usize, relative: &Path) -> Result<Vec<Entry>, VfsError> {
        let (fs, root) = if pane == 0 {
            (&mut *self.local, self.local_root)
        } else {
            (&mut *self.remote, self.remote_root)
        };
        match fs.list(&root.join(relative)) {
            Ok(entries) => Ok(entries),
            Err(e) if e.is_not_found() => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }
}

/// What each direction cannot preserve.
struct Caveats {
    to_remote: Option<&'static str>,
    to_local: Option<&'static str>,
}

impl Caveats {
    const fn for_target(&self, pane: usize) -> Option<&'static str> {
        if pane == 0 {
            self.to_local
        } else {
            self.to_remote
        }
    }
}

/// What writing from `source` to `dest` cannot carry over.
fn caveat(source: Capabilities, dest: Capabilities) -> Option<&'static str> {
    if !dest.set_mtime && source.mtime {
        return Some("the destination cannot set timestamps");
    }
    if !dest.mode && source.mode {
        return Some("the destination has no permission bits");
    }
    None
}

// The recursive worker genuinely needs all of this; bundling further would
// only move the same fields somewhere else.
#[allow(clippy::too_many_arguments)]
fn walk(
    sides: &mut Sides<'_>,
    relative: &Path,
    depth: usize,
    comparator: &Comparator,
    snapshot: &Snapshot,
    config: &Config,
    filters: &Filters,
    policy: &super::safety::DeletePolicy,
    caveats: &Caveats,
    first_run: bool,
    built: &mut Plan,
) {
    if depth > MAX_DEPTH {
        built.unreadable.push((
            relative.to_path_buf(),
            format!("stopped at {MAX_DEPTH} levels deep; a symlink loop?"),
        ));
        return;
    }

    // Either side being unreadable stops this subtree. Unlike a one-way sync,
    // there is no "safe direction" to continue in: every row here could be a
    // deletion on the side we *can* read, justified by an absence we never
    // actually observed.
    let mut listings = [Vec::new(), Vec::new()];
    for pane in [0, 1] {
        match sides.list(pane, relative) {
            Ok(entries) => listings[pane] = entries,
            Err(e) => {
                let side = if pane == 0 { "local" } else { "remote" };
                built
                    .unreadable
                    .push((relative.to_path_buf(), format!("{side}: {e}")));
                return;
            }
        }
    }
    let [local_entries, remote_entries] = listings;

    // The union of both sides' names, in a stable order. A name on one side
    // only is the interesting case in a two-way sync, which is why this is a
    // union rather than a walk of one side with lookups into the other.
    let mut names: Vec<&str> = local_entries
        .iter()
        .chain(remote_entries.iter())
        .map(|e| e.name.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();

    for name in names {
        let local = local_entries.iter().find(|e| e.name == name);
        let remote = remote_entries.iter().find(|e| e.name == name);
        let child = relative.join(name);

        // Hidden files, per config. Checked on whichever side has it: a
        // dotfile is a dotfile regardless of which end it came from.
        if let Some(entry) = local.or(remote) {
            if !config.show_hidden && entry.is_hidden() {
                continue;
            }
        }

        // lazysync's own backup directory is bookkeeping, not content. Syncing
        // it would copy superseded versions back as though they were edits --
        // and in a two-way sync, back onto the side they were superseded on.
        if super::safety::is_backup_path(policy, &child) {
            continue;
        }

        let is_dir = local
            .or(remote)
            .is_some_and(|e| matches!(e.kind, EntryKind::Dir) || e.symlink_to_dir);
        if filters.excludes(&child, is_dir) {
            continue;
        }

        // Symlinks are visible and not transferred, as in a one-way sync:
        // `Vfs` has no symlink operation, and both alternatives -- following
        // the link, or dropping it -- change the tree in ways nobody asked
        // for.
        if local
            .iter()
            .chain(remote.iter())
            .any(|e| matches!(e.kind, EntryKind::Symlink))
        {
            let entry = local.or(remote).expect("one side has it");
            built.rows.push(Row {
                caveat: Some("symlink, not transferred"),
                included: false,
                ..plan::compare_row(
                    child,
                    entry,
                    entry,
                    Reason::SameSizeNoTimestamps,
                    comparator.method(),
                    1,
                )
            });
            continue;
        }

        match (local.map(|e| e.kind), remote.map(|e| e.kind)) {
            // A directory on both sides, or on one side only: recurse. The
            // side that lacks it gets a `CreateDir` row, and the walk below
            // plans its contents.
            (Some(EntryKind::Dir), Some(EntryKind::Dir)) => {
                recurse(
                    sides, &child, depth, comparator, snapshot, config, filters, policy, caveats,
                    first_run, built,
                );
            }
            (Some(EntryKind::Dir), None) => {
                let entry = local.expect("matched");
                built.rows.push(plan::create_row(
                    child.clone(),
                    entry,
                    comparator.method(),
                    1,
                ));
                recurse(
                    sides, &child, depth, comparator, snapshot, config, filters, policy, caveats,
                    first_run, built,
                );
            }
            (None, Some(EntryKind::Dir)) => {
                let entry = remote.expect("matched");
                built.rows.push(plan::create_row(
                    child.clone(),
                    entry,
                    comparator.method(),
                    0,
                ));
                recurse(
                    sides, &child, depth, comparator, snapshot, config, filters, policy, caveats,
                    first_run, built,
                );
            }

            // One side has a directory and the other a file. Replacing either
            // with the other is a bigger change than a sync should make on its
            // own initiative, so it is shown and excluded.
            (Some(EntryKind::Dir), Some(_)) | (Some(_), Some(EntryKind::Dir)) => {
                let entry = local.expect("both sides present");
                built.rows.push(Row {
                    caveat: Some(
                        "type differs: one side is a directory. Not transferred -- resolve it \
                         by hand",
                    ),
                    included: false,
                    ..plan::compare_row(
                        child,
                        entry,
                        remote.expect("both sides present"),
                        Reason::SizeDiffers,
                        comparator.method(),
                        1,
                    )
                });
            }

            // Files, or an absence. The classifier decides.
            _ => {
                let verdict = classify(
                    local.map(Observed::from_entry),
                    remote.map(Observed::from_entry),
                    snapshot.get(&child),
                    comparator,
                    first_run,
                );

                if let Some(row) = row_for(verdict, &child, local, remote, comparator, caveats) {
                    built.rows.push(row);
                }
            }
        }
    }
}

// Threading the same arguments one level down; a wrapper purely to keep the
// call sites above readable.
#[allow(clippy::too_many_arguments)]
fn recurse(
    sides: &mut Sides<'_>,
    child: &Path,
    depth: usize,
    comparator: &Comparator,
    snapshot: &Snapshot,
    config: &Config,
    filters: &Filters,
    policy: &super::safety::DeletePolicy,
    caveats: &Caveats,
    first_run: bool,
    built: &mut Plan,
) {
    walk(
        sides,
        child,
        depth + 1,
        comparator,
        snapshot,
        config,
        filters,
        policy,
        caveats,
        first_run,
        built,
    );
}

/// Turn a verdict into a plan row.
///
/// `None` where there is nothing to show: a path in agreement, or one gone
/// from both sides. Those still matter to the *snapshot*, which is updated
/// after a successful run rather than from the plan.
fn row_for(
    verdict: Verdict,
    child: &Path,
    local: Option<&Entry>,
    remote: Option<&Entry>,
    comparator: &Comparator,
    caveats: &Caveats,
) -> Option<Row> {
    match verdict {
        // An agreeing path still gets a row, so the window's "Skipped,
        // identical" section is as informative as in a one-way sync.
        Verdict::InAgreement => {
            let (local, remote) = (local?, remote?);
            Some(plan::compare_row(
                child.to_path_buf(),
                local,
                remote,
                Reason::BothChangedAlike,
                comparator.method(),
                1,
            ))
        }

        Verdict::Copy {
            source_pane,
            reason,
        } => {
            let target_pane = 1 - source_pane;
            let source = if source_pane == 0 { local? } else { remote? };
            let existing = if target_pane == 0 { local } else { remote };

            let mut row = match existing {
                Some(dest) => plan::compare_row(
                    child.to_path_buf(),
                    source,
                    dest,
                    reason,
                    comparator.method(),
                    target_pane,
                ),
                None => plan::create_row(
                    child.to_path_buf(),
                    source,
                    comparator.method(),
                    target_pane,
                ),
            };
            // `compare_row` derives the action from whether the reason
            // `differs`, which is right for every bisync reason that reaches
            // here -- but the reason is the bisync one, so set it explicitly
            // rather than relying on that coincidence.
            row.reason = reason;
            row.caveat = caveats.for_target(target_pane);
            Some(row)
        }

        Verdict::Propagate { target_pane } => {
            let doomed = if target_pane == 0 { local? } else { remote? };
            let mut row = plan::delete_row(
                child.to_path_buf(),
                doomed,
                comparator.method(),
                target_pane,
            );
            row.reason = Reason::DeletedOnOneSide;
            Some(row)
        }

        Verdict::Conflict { reason } => Some(plan::conflict_row(
            child.to_path_buf(),
            local,
            remote,
            reason,
            comparator.method(),
        )),

        Verdict::GoneFromBoth => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::{
        config::{Compare as CompareConfig, CompareMethod},
        vfs::Capabilities,
    };

    fn caps() -> Capabilities {
        Capabilities {
            mtime: true,
            set_mtime: true,
            mode: true,
            symlinks: true,
            ranged_writes: true,
            atomic_overwrite_rename: true,
            mtime_granularity_secs: 1,
        }
    }

    fn comparator() -> Comparator {
        Comparator::new(
            &CompareConfig {
                method: CompareMethod::SizeMtime,
                mtime_tolerance_secs: 2,
                use_provider_hash: true,
            },
            caps(),
            caps(),
        )
    }

    fn at(size: u64, epoch: i64) -> Observed {
        Observed {
            size,
            mtime: Some(Utc.timestamp_opt(epoch, 0).unwrap()),
        }
    }

    /// A snapshot record where both sides agree.
    fn was(size: u64, epoch: i64) -> Record {
        let mtime = Some(Utc.timestamp_opt(epoch, 0).unwrap());
        Record {
            local_size: size,
            local_mtime: mtime,
            remote_size: size,
            remote_mtime: mtime,
        }
    }

    fn judge(local: Option<Observed>, remote: Option<Observed>, was: Option<&Record>) -> Verdict {
        classify(local, remote, was, &comparator(), false)
    }

    #[test]
    fn untouched_on_both_sides_is_agreement() {
        let record = was(10, 1000);
        assert_eq!(
            judge(Some(at(10, 1000)), Some(at(10, 1000)), Some(&record)),
            Verdict::InAgreement
        );
    }

    #[test]
    fn changed_on_one_side_copies_that_way() {
        let record = was(10, 1000);

        assert_eq!(
            judge(Some(at(20, 2000)), Some(at(10, 1000)), Some(&record)),
            Verdict::Copy {
                source_pane: 0,
                reason: Reason::ChangedOnOneSide
            },
            "the local side changed, so it wins"
        );
        assert_eq!(
            judge(Some(at(10, 1000)), Some(at(20, 2000)), Some(&record)),
            Verdict::Copy {
                source_pane: 1,
                reason: Reason::ChangedOnOneSide
            },
            "and the other way round"
        );
    }

    /// The case the whole design is for: no guessing by timestamp.
    #[test]
    fn changed_on_both_sides_is_a_conflict() {
        let record = was(10, 1000);

        let verdict = judge(Some(at(20, 5000)), Some(at(30, 2000)), Some(&record));
        assert_eq!(
            verdict,
            Verdict::Conflict {
                reason: Reason::BothChanged
            },
            "picking the newer file would silently discard the other edit, and 'newer' is a \
             claim about two clocks"
        );
    }

    /// Two sides that edited to the same result have converged. Reporting a
    /// conflict there would be noise the user cannot act on.
    #[test]
    fn changed_on_both_sides_to_the_same_content_is_agreement() {
        let record = was(10, 1000);
        assert_eq!(
            judge(Some(at(20, 4000)), Some(at(20, 4000)), Some(&record)),
            Verdict::InAgreement
        );
    }

    #[test]
    fn new_on_one_side_is_copied() {
        assert_eq!(
            judge(Some(at(10, 1000)), None, None),
            Verdict::Copy {
                source_pane: 0,
                reason: Reason::NewOnOneSide
            }
        );
        assert_eq!(
            judge(None, Some(at(10, 1000)), None),
            Verdict::Copy {
                source_pane: 1,
                reason: Reason::NewOnOneSide
            }
        );
    }

    #[test]
    fn new_on_both_sides_with_different_content_is_a_conflict() {
        assert_eq!(
            judge(Some(at(10, 1000)), Some(at(20, 1000)), None),
            Verdict::Conflict {
                reason: Reason::BothNew
            }
        );
    }

    #[test]
    fn new_on_both_sides_with_the_same_content_is_agreement() {
        assert_eq!(
            judge(Some(at(10, 1000)), Some(at(10, 1000)), None),
            Verdict::InAgreement
        );
    }

    /// The thing a snapshot exists to make possible.
    #[test]
    fn a_deletion_propagates_when_the_other_side_is_untouched() {
        let record = was(10, 1000);

        assert_eq!(
            judge(None, Some(at(10, 1000)), Some(&record)),
            Verdict::Propagate { target_pane: 1 },
            "deleted locally and untouched remotely, so it goes from the remote too"
        );
        assert_eq!(
            judge(Some(at(10, 1000)), None, Some(&record)),
            Verdict::Propagate { target_pane: 0 },
        );
    }

    /// Deleted one side, edited the other. Propagating the deletion throws away
    /// an edit; propagating the edit resurrects a file somebody removed.
    #[test]
    fn a_deletion_against_an_edit_is_a_conflict() {
        let record = was(10, 1000);

        assert_eq!(
            judge(None, Some(at(99, 9000)), Some(&record)),
            Verdict::Conflict {
                reason: Reason::DeletedVersusChanged
            }
        );
        assert_eq!(
            judge(Some(at(99, 9000)), None, Some(&record)),
            Verdict::Conflict {
                reason: Reason::DeletedVersusChanged
            }
        );
    }

    #[test]
    fn gone_from_both_sides_needs_nothing() {
        let record = was(10, 1000);
        assert_eq!(judge(None, None, Some(&record)), Verdict::GoneFromBoth);
        assert_eq!(judge(None, None, None), Verdict::GoneFromBoth);
    }

    /// **The first-run safety property.** With no snapshot, nothing may be
    /// deleted -- a path on one side only is as likely to be new as removed,
    /// and guessing wrong destroys data.
    #[test]
    fn a_first_run_never_propagates_a_deletion() {
        let record = was(10, 1000);
        let comparator = comparator();

        for (local, remote) in [(None, Some(at(10, 1000))), (Some(at(10, 1000)), None)] {
            let verdict = classify(local, remote, Some(&record), &comparator, true);
            assert!(
                matches!(verdict, Verdict::Copy { .. }),
                "a first run must copy rather than delete, got {verdict:?}"
            );
        }
    }

    /// And with a snapshot, the same inputs *do* delete -- otherwise the
    /// snapshot would be pointless and deletions would never propagate at all.
    #[test]
    fn a_later_run_does_propagate_the_same_deletion() {
        let record = was(10, 1000);
        let verdict = classify(
            None,
            Some(at(10, 1000)),
            Some(&record),
            &comparator(),
            false,
        );
        assert_eq!(verdict, Verdict::Propagate { target_pane: 1 });
    }

    /// A timestamp difference inside the tolerance is not a change. Were it
    /// one, a sync to a coarse-grained backend would make every file look
    /// edited on both sides -- turning a quiet tree into a wall of conflicts.
    #[test]
    fn a_difference_within_tolerance_is_not_a_change() {
        let record = was(10, 1000);
        assert_eq!(
            judge(Some(at(10, 1001)), Some(at(10, 999)), Some(&record)),
            Verdict::InAgreement
        );
    }

    /// A side whose backend reports no timestamp must not read as changed on
    /// every run. Equal size is all the evidence there is.
    #[test]
    fn a_missing_timestamp_is_not_evidence_of_change() {
        let record = Record {
            local_size: 10,
            local_mtime: Some(Utc.timestamp_opt(1000, 0).unwrap()),
            remote_size: 10,
            remote_mtime: None,
        };

        let remote_without_time = Observed {
            size: 10,
            mtime: None,
        };

        assert_eq!(
            judge(Some(at(10, 1000)), Some(remote_without_time), Some(&record)),
            Verdict::InAgreement
        );
    }

    /// Every combination of the three inputs produces a verdict, and no
    /// combination produces a deletion on a first run. The exhaustive version
    /// of the property above.
    #[test]
    fn every_combination_is_classified_and_first_runs_never_delete() {
        let record = was(10, 1000);
        let comparator = comparator();
        let states = [None, Some(at(10, 1000)), Some(at(77, 7000))];

        for local in states {
            for remote in states {
                for history in [None, Some(&record)] {
                    for first_run in [true, false] {
                        let verdict = classify(local, remote, history, &comparator, first_run);

                        if first_run {
                            assert!(
                                !matches!(verdict, Verdict::Propagate { .. }),
                                "first run must not delete: {local:?} {remote:?} \
                                 {history:?} -> {verdict:?}"
                            );
                        }

                        // A conflict must never be reported for a path only
                        // one side has: there is nothing to weigh against.
                        if matches!(verdict, Verdict::Conflict { .. }) {
                            assert!(
                                local.is_some() || remote.is_some(),
                                "a conflict needs at least one side to exist"
                            );
                        }
                    }
                }
            }
        }
    }
}
