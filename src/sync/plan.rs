//! What a sync would do, before it does any of it.
//!
//! A [`Plan`] is the whole output of the planning stage: every file that would
//! be created, overwritten, deleted or skipped, each with *both* sides'
//! size and timestamp and the reason it was judged different. The
//! confirmation window renders this, and the executor applies exactly this --
//! so the plan is the dry run, and there is no second code path whose
//! behaviour could drift from what you were shown.
//!
//! Rows carry an `included` flag rather than being filtered out, which is what
//! lets the window exclude one with a keystroke and recompute the totals
//! without re-walking the trees.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::compare::{Method, Reason};
use crate::vfs::{Entry, EntryKind};

/// What would happen to one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Action {
    /// Not on the destination yet.
    Create,
    /// Present on both, and different.
    Overwrite,
    /// On the destination and not the source. Only in a mirror.
    Delete,
    /// Present on both and judged the same.
    Skip,
    /// A directory that has to exist before its contents can be written.
    ///
    /// Separate from `Create` because it transfers no bytes and is not
    /// something the user needs to review individually.
    CreateDir,
    /// Both sides changed since the last sync, and lazysync will not guess.
    ///
    /// Only bisync produces these. A one-directional sync has an answer by
    /// definition -- the source wins -- but in a two-way sync picking the
    /// newer file silently discards the other edit, and "newer" is a claim
    /// about two clocks that may not agree. So it carries a
    /// [`Resolution`] the user supplies, and the plan refuses to apply while
    /// any included conflict is still [`Resolution::Pending`].
    Conflict,
}

/// What to do about a conflict.
///
/// Resolution lives on the row rather than being applied by rewriting it,
/// because the row is what the window shows and what the executor reads: a
/// decision recorded anywhere else would be a second source of truth about
/// what is to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Undecided. Blocks the whole plan rather than defaulting, because every
    /// possible default here loses somebody's work.
    Pending,
    /// The local copy wins and is written to the remote.
    KeepLocal,
    /// The remote copy wins and is written locally.
    KeepRemote,
    /// Neither loses: the other side's version is kept under a suffixed name
    /// alongside the winner.
    KeepBoth,
    /// Leave both sides exactly as they are this run. The conflict will be
    /// presented again next time, which is the honest outcome of deferring.
    Skip,
}

impl Resolution {
    /// The pane this resolution writes to, if it writes at all.
    ///
    /// `KeepBoth` writes to the remote, like `KeepLocal`: the difference is
    /// not *where* but what happens to the file being displaced. Keeping each
    /// side's own version at the original name would leave the pair
    /// permanently different and raise the same conflict on every later run,
    /// so instead the local copy wins on both sides and the remote's old
    /// version is renamed aside next to itself. Nothing is lost and the pair
    /// converges. See [`Resolution::preserves_displaced`].
    pub const fn target_pane(self) -> Option<usize> {
        match self {
            // Local wins, so the remote is what changes.
            Self::KeepLocal | Self::KeepBoth => Some(1),
            Self::KeepRemote => Some(0),
            Self::Pending | Self::Skip => None,
        }
    }

    /// Whether the file being overwritten is renamed aside rather than
    /// replaced.
    pub const fn preserves_displaced(self) -> bool {
        matches!(self, Self::KeepBoth)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "undecided",
            Self::KeepLocal => "keep local",
            Self::KeepRemote => "keep remote",
            Self::KeepBoth => "keep both",
            Self::Skip => "leave alone",
        }
    }
}

impl Action {
    /// The glyph shown in the window's first column.
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Create => "+",
            // Direction is decided per row by `Row::clobbers_newer`, since
            // which way the arrow points is the whole point of the column.
            Self::Overwrite => "\u{2191}",
            Self::Delete => "\u{2717}",
            Self::Skip => "=",
            Self::CreateDir => "+",
            Self::Conflict => "\u{26a0}",
        }
    }

    /// The section heading this action is grouped under.
    pub const fn section(self) -> Section {
        match self {
            Self::Create | Self::CreateDir => Section::Create,
            Self::Overwrite => Section::Overwrite,
            Self::Delete => Section::Delete,
            Self::Skip => Section::Skipped,
            Self::Conflict => Section::Conflict,
        }
    }
}

/// A heading in the confirmation window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Create,
    Overwrite,
    Delete,
    /// Both sides changed. Needs a decision before the plan can run.
    Conflict,
    Skipped,
}

impl Section {
    /// Every section, in display order.
    ///
    /// Deletions come after overwrites and before the skipped list, so the
    /// destructive part of a plan is never below a fold of two hundred
    /// unchanged files. Conflicts sit last of the actionable sections because
    /// they are the ones you have to act on: nothing runs until they are
    /// resolved, so they should be what the eye lands on.
    pub const ALL: [Self; 5] = [
        Self::Create,
        Self::Overwrite,
        Self::Delete,
        Self::Conflict,
        Self::Skipped,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Overwrite => "Overwrite",
            Self::Delete => "Delete",
            Self::Conflict => "Conflicts",
            Self::Skipped => "Skipped, identical",
        }
    }

    /// Whether this section's rows destroy data if applied.
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::Delete | Self::Overwrite | Self::Conflict)
    }
}

/// One file's worth of plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Path relative to the sync roots, which is what both sides share.
    pub path: PathBuf,
    pub action: Action,

    /// Which pane this row *writes to* -- 0 local, 1 remote.
    ///
    /// Per row rather than per plan, because bisync moves files in both
    /// directions within one plan and the whole design rests on there being
    /// exactly one plan that is both shown and applied. A one-directional mode
    /// simply produces rows that all agree.
    ///
    /// Defined as the side that *changes*, which makes it meaningful for every
    /// action: for a create or overwrite it is where the bytes land, and for a
    /// delete it is where the file is removed. [`Row::source_pane`] is the
    /// other one.
    pub target_pane: usize,
    /// Why the two sides were judged different, or the same.
    pub reason: Reason,
    /// How they were compared, so the window can say which rule applied.
    pub method: Method,

    /// Size and time on the source side, absent for a deletion.
    pub source: Option<Side>,
    /// Size and time on the destination side, absent for a creation.
    pub dest: Option<Side>,

    /// Whether this row will be applied. Toggled in the window.
    pub included: bool,

    /// How a conflict is to be settled. `None` for every other action.
    ///
    /// `Some(Resolution::Pending)` is the state a fresh conflict is in, and
    /// the plan refuses to apply while any included row is in it.
    pub resolution: Option<Resolution>,

    /// Set when the backend cannot preserve something the transfer implies --
    /// a timestamp on a destination that cannot set one, say. Shown on the
    /// row, because a silently-not-preserved mtime makes the *next* sync
    /// re-copy the file.
    pub caveat: Option<&'static str>,
}

/// One side's view of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Side {
    pub size: u64,
    pub mtime: Option<DateTime<Utc>>,
}

impl Side {
    pub fn from_entry(entry: &Entry) -> Self {
        Self {
            size: entry.size,
            mtime: entry.mtime,
        }
    }
}

impl Row {
    /// The pane this row's bytes come from. See [`Row::target_pane`].
    pub const fn source_pane(&self) -> usize {
        1 - self.target_pane
    }

    /// This row's view of one pane, by pane index.
    ///
    /// The window lays out its two columns by *pane* -- local on the left,
    /// remote on the right -- while a row stores its sides by role. In a
    /// bisync plan those differ from row to row, so rendering by role would
    /// put the local file in the right-hand column for every row that happens
    /// to be pulling.
    pub const fn side_of(&self, pane: usize) -> Option<Side> {
        if pane == self.target_pane {
            self.dest
        } else {
            self.source
        }
    }

    /// Whether applying this row replaces newer data with older.
    pub const fn clobbers_newer(&self) -> bool {
        self.reason.clobbers_newer()
    }

    /// Bytes this row transfers if applied.
    pub const fn bytes(&self) -> u64 {
        if matches!(self.action, Action::Conflict) {
            // A conflict's winning side is whichever the resolution names, so
            // the byte count follows the decision rather than the row's
            // nominal source. Undecided means no number, because there is no
            // answer yet.
            return match self.resolution {
                Some(Resolution::KeepLocal) => match self.side_of(0) {
                    Some(side) => side.size,
                    None => 0,
                },
                Some(Resolution::KeepRemote) => match self.side_of(1) {
                    Some(side) => side.size,
                    None => 0,
                },
                Some(Resolution::KeepBoth) => {
                    let local = match self.side_of(0) {
                        Some(side) => side.size,
                        None => 0,
                    };
                    let remote = match self.side_of(1) {
                        Some(side) => side.size,
                        None => 0,
                    };
                    local.saturating_add(remote)
                }
                _ => 0,
            };
        }

        match (self.action, &self.source) {
            (Action::Create | Action::Overwrite, Some(side)) => side.size,
            _ => 0,
        }
    }

    /// Bytes this row removes if applied.
    pub const fn bytes_deleted(&self) -> u64 {
        match (self.action, &self.dest) {
            (Action::Delete, Some(side)) => side.size,
            _ => 0,
        }
    }

    /// How much bigger the destination gets, as a signed delta.
    pub const fn size_delta(&self) -> i128 {
        let source = match &self.source {
            Some(side) => side.size as i128,
            None => 0,
        };
        let dest = match &self.dest {
            Some(side) => side.size as i128,
            None => 0,
        };
        source - dest
    }

    /// Whether this row is worth the user's attention.
    ///
    /// Directory creations and identical files are not: a plan whose review
    /// list is nine-tenths `=` rows is a plan nobody reads.
    pub const fn needs_review(&self) -> bool {
        !matches!(self.action, Action::Skip | Action::CreateDir)
    }

    /// Whether this row is a conflict still awaiting a decision.
    pub const fn is_undecided(&self) -> bool {
        matches!(self.resolution, Some(Resolution::Pending))
    }

    /// Whether applying this row moves any bytes.
    ///
    /// A conflict resolved to [`Resolution::Skip`] does not, which is the
    /// difference between deferring a decision and making one.
    pub const fn is_effective(&self) -> bool {
        match self.action {
            Action::Conflict => matches!(
                self.resolution,
                Some(Resolution::KeepLocal | Resolution::KeepRemote | Resolution::KeepBoth)
            ),
            Action::Create | Action::Overwrite | Action::Delete | Action::CreateDir => true,
            Action::Skip => false,
        }
    }
}

/// Everything a sync would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Every row, in section order then path order.
    pub rows: Vec<Row>,

    /// Where the files come from and go, for the window's header.
    pub source_root: PathBuf,
    pub dest_root: PathBuf,
    pub source_label: String,
    pub dest_label: String,

    /// How the comparison was done, for the header.
    pub method: Method,
    pub tolerance_secs: i64,

    /// How many top-level names the plan was limited to, when it was built
    /// from the source pane's marks rather than from the whole directory.
    ///
    /// The window has to say so. A plan that covers four marked files looks
    /// exactly like a plan for a directory that happens to contain four
    /// changed files, and the difference matters: in the first, everything
    /// else was never examined, so "nothing else to do" is not something the
    /// window would be entitled to imply.
    pub scoped_to: Option<usize>,

    /// Directories that could not be read, with why.
    ///
    /// Kept rather than aborting: a plan covering the readable 95% of a tree
    /// is useful, and a silent gap is not. The window shows these so an
    /// unreadable directory cannot be mistaken for an empty one.
    pub unreadable: Vec<(PathBuf, String)>,

    /// Set when a safety rail refused the plan. The window shows it and
    /// refuses to apply.
    pub refusal: Option<String>,

    /// Set when the user stopped the scan before it finished.
    ///
    /// Such a plan covers part of a tree with no way to tell which part, so
    /// it is thrown away rather than shown. The flag exists so the caller can
    /// tell "you cancelled" from "there was nothing to do" -- two outcomes
    /// that otherwise both produce an empty plan and would read identically.
    pub cancelled: bool,

    /// Whether this plan moves files in both directions.
    ///
    /// The window needs it for the header -- `⇄` rather than `→`, since
    /// "local → remote" would be a false description of a bisync -- and the
    /// safety check needs it to apply its delete limits to *both* sides rather
    /// than only the nominal destination.
    pub bidirectional: bool,

    /// Set on a first bisync, when there is no snapshot to compare against.
    ///
    /// Shown in the window, because the run behaves differently: nothing can
    /// be deleted, since a path on one side only is as likely to be new as
    /// removed.
    pub first_run: bool,
}

impl Plan {
    /// Rows in one section that are worth reviewing.
    ///
    /// Directory creations are excluded: they transfer nothing, there is no
    /// decision to make about them, and counting them made the "Create (2)"
    /// heading disagree with the "2 ops" footer. The footer discloses them
    /// separately via [`Totals::directories`].
    pub fn section(&self, section: Section) -> impl Iterator<Item = &Row> {
        // Only `CreateDir` is filtered, not everything `needs_review` is
        // false for: the skipped-identical list is the point of its own
        // section, so excluding `Skip` here would empty it.
        self.rows
            .iter()
            .filter(move |r| r.action.section() == section && r.action != Action::CreateDir)
    }

    pub fn count(&self, section: Section) -> usize {
        self.section(section).count()
    }

    /// Every row in a section, directories included. For the executor.
    pub fn section_all(&self, section: Section) -> impl Iterator<Item = &Row> {
        self.rows
            .iter()
            .filter(move |r| r.action.section() == section)
    }

    /// Rows that would actually be applied.
    pub fn included(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|r| r.included)
    }

    /// Whether anything would happen at all.
    pub fn is_empty(&self) -> bool {
        !self.rows.iter().any(Row::needs_review)
    }

    /// Conflicts still awaiting a decision.
    pub fn undecided(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|r| r.included && r.is_undecided())
    }

    /// Whether applying is allowed.
    ///
    /// An undecided conflict blocks the *whole* plan rather than being skipped
    /// along with its own row. Running the other ninety-nine rows and quietly
    /// leaving one unresolved would mean the next run compares against a tree
    /// that is half-synced, and the user would have no indication that
    /// anything was left behind. Exclude the row with `space` to say so
    /// deliberately.
    pub fn can_apply(&self) -> bool {
        self.refusal.is_none()
            && self.undecided().next().is_none()
            && self
                .included()
                .any(|r| r.needs_review() && r.is_effective())
    }

    /// Totals for the window's footer.
    pub fn totals(&self) -> Totals {
        let mut totals = Totals::default();

        for row in self.included() {
            match row.action {
                // Saturating, not wrapping: these are summed while drawing a
                // frame, and a panic in a debug build is a crash rather than a
                // wrong number.
                Action::Create => {
                    totals.creates += 1;
                    totals.bytes_out = totals.bytes_out.saturating_add(row.bytes());
                }
                Action::Overwrite => {
                    totals.overwrites += 1;
                    totals.bytes_out = totals.bytes_out.saturating_add(row.bytes());
                    if row.clobbers_newer() {
                        totals.clobbers_newer += 1;
                    }
                }
                Action::Delete => {
                    totals.deletes += 1;
                    totals.bytes_deleted = totals.bytes_deleted.saturating_add(row.bytes_deleted());
                }
                Action::CreateDir => totals.directories += 1,
                Action::Skip => totals.skipped += 1,

                // A conflict counts as an operation only once it has a
                // resolution that moves something. Counting an undecided one
                // in `bytes_out` would put a number in the footer for work
                // whose direction is not yet known -- and the plan cannot run
                // at all while one exists.
                Action::Conflict => {
                    totals.conflicts += 1;
                    match row.resolution {
                        Some(Resolution::Pending) => totals.undecided += 1,
                        // `Row::bytes` reads the resolution, including the
                        // keep-both case where each side's version crosses to
                        // the other.
                        Some(
                            Resolution::KeepLocal | Resolution::KeepRemote | Resolution::KeepBoth,
                        ) => {
                            totals.conflict_ops += 1;
                            totals.bytes_out = totals.bytes_out.saturating_add(row.bytes());
                        }
                        Some(Resolution::Skip) | None => {}
                    }
                }
            }
        }

        totals
    }

    /// Every row that would destroy data, for the safety check.
    pub fn destructive_rows(&self) -> impl Iterator<Item = &Row> {
        self.rows
            .iter()
            .filter(|r| r.included && matches!(r.action, Action::Delete))
    }

    /// Toggle one row's inclusion, by index into [`Plan::rows`].
    pub fn toggle(&mut self, index: usize) {
        if let Some(row) = self.rows.get_mut(index) {
            row.included = !row.included;
        }
    }

    /// Include or exclude every reviewable row.
    pub fn set_all(&mut self, included: bool) {
        for row in &mut self.rows {
            if row.needs_review() {
                row.included = included;
            }
        }
    }

    /// Invert which reviewable rows are included.
    pub fn invert(&mut self) {
        for row in &mut self.rows {
            if row.needs_review() {
                row.included = !row.included;
            }
        }
    }

    /// Include only the row at `index`.
    pub fn only(&mut self, index: usize) {
        for (i, row) in self.rows.iter_mut().enumerate() {
            if row.needs_review() {
                row.included = i == index;
            }
        }
    }

    /// Sort into display order: sections in [`Section::ALL`] order, paths
    /// alphabetically within each.
    pub fn sort(&mut self) {
        self.rows.sort_by(|a, b| {
            a.action
                .section()
                .cmp(&b.action.section())
                .then_with(|| a.path.cmp(&b.path))
        });
    }

    /// The directories that must exist before `included` rows can be written.
    ///
    /// Derived from the rows rather than tracked separately, so a row excluded
    /// in the window cannot leave behind an empty directory that was only
    /// created for it.
    /// Directories to create, paired with the pane each belongs on.
    ///
    /// The pane matters because bisync writes both ways in one plan: a pull
    /// needs its parent directories locally and a push needs them on the
    /// remote, and creating every one on both sides would leave empty
    /// directories on whichever side was not receiving anything.
    pub fn required_directories(&self) -> Vec<(usize, PathBuf)> {
        let mut dirs: Vec<(usize, PathBuf)> = Vec::new();

        for row in self.included() {
            if matches!(row.action, Action::Delete | Action::Skip) {
                continue;
            }
            if matches!(row.action, Action::Conflict) && !row.is_effective() {
                continue;
            }

            let target = if matches!(row.action, Action::CreateDir) {
                Some(row.path.as_path())
            } else {
                row.path.parent()
            };

            // Which side needs them. A conflict resolved to keep-both writes
            // in both directions, so it needs its parents on both.
            let panes: &[usize] = match (row.action, row.resolution) {
                (Action::Conflict, Some(Resolution::KeepBoth)) => &[0, 1],
                (Action::Conflict, Some(Resolution::KeepLocal)) => &[1],
                (Action::Conflict, Some(Resolution::KeepRemote)) => &[0],
                _ => match row.target_pane {
                    0 => &[0],
                    _ => &[1],
                },
            };

            for &pane in panes {
                let mut current = target;
                while let Some(dir) = current {
                    if dir.as_os_str().is_empty() {
                        break;
                    }
                    let item = (pane, dir.to_path_buf());
                    if !dirs.contains(&item) {
                        dirs.push(item);
                    }
                    current = dir.parent();
                }
            }
        }

        // Shallowest first, so a parent exists before its child is created.
        dirs.sort_by_key(|(pane, dir)| (dir.components().count(), *pane));
        dirs
    }
}

/// The footer line of the confirmation window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub creates: usize,
    pub overwrites: usize,
    pub deletes: usize,
    pub directories: usize,
    pub skipped: usize,
    /// Overwrites that replace newer data with older.
    pub clobbers_newer: usize,
    /// Rows where both sides changed. Bisync only.
    pub conflicts: usize,
    /// Of those, how many still have no decision. Nothing runs while this is
    /// above zero, so the footer says it plainly.
    pub undecided: usize,
    /// Conflicts resolved to something that actually moves a file.
    pub conflict_ops: usize,
    pub bytes_out: u64,
    pub bytes_deleted: u64,
}

impl Totals {
    /// Operations that transfer or remove something.
    ///
    /// Resolved conflicts count, because they move files; undecided ones and
    /// those deferred with `leave alone` do not. A footer reading "0 ops"
    /// while bytes crossed the wire would be exactly the kind of disagreement
    /// between the shown plan and the applied one that this design exists to
    /// prevent.
    pub const fn operations(&self) -> usize {
        self.creates + self.overwrites + self.deletes + self.conflict_ops
    }
}

/// Build a row for a file present only on the source.
pub fn create_row(path: PathBuf, source: &Entry, method: Method, target_pane: usize) -> Row {
    Row {
        path,
        target_pane,
        action: if matches!(source.kind, EntryKind::Dir) {
            Action::CreateDir
        } else {
            Action::Create
        },
        // Nothing to compare against; the row exists because the destination
        // has no such file.
        reason: Reason::NotOnDestination,
        method,
        source: Some(Side::from_entry(source)),
        dest: None,
        included: true,
        resolution: None,
        caveat: None,
    }
}

/// Build a row for a file present on both sides.
pub fn compare_row(
    path: PathBuf,
    source: &Entry,
    dest: &Entry,
    reason: Reason,
    method: Method,
    target_pane: usize,
) -> Row {
    Row {
        path,
        target_pane,
        action: if reason.differs() {
            Action::Overwrite
        } else {
            Action::Skip
        },
        reason,
        method,
        source: Some(Side::from_entry(source)),
        dest: Some(Side::from_entry(dest)),
        included: true,
        resolution: None,
        caveat: None,
    }
}

/// Build a row for a file present only on the destination.
pub fn delete_row(path: PathBuf, dest: &Entry, method: Method, target_pane: usize) -> Row {
    Row {
        path,
        target_pane,
        action: Action::Delete,
        reason: Reason::NotOnSource,
        method,
        source: None,
        dest: Some(Side::from_entry(dest)),
        included: true,
        resolution: None,
        caveat: None,
    }
}

/// Build a conflict row: both sides changed since the last sync.
///
/// `target_pane` is set to the remote, arbitrarily, so that the field is never
/// meaningless -- but nothing reads it until a [`Resolution`] supplies the
/// real answer, and [`Plan::can_apply`] refuses while it is
/// [`Resolution::Pending`]. Defaulting the *resolution* instead would be the
/// mistake: there is no safe default when both sides have edits.
pub fn conflict_row(
    path: PathBuf,
    local: Option<&Entry>,
    remote: Option<&Entry>,
    reason: Reason,
    method: Method,
) -> Row {
    Row {
        path,
        action: Action::Conflict,
        target_pane: 1,
        reason,
        method,
        // Stored by pane, not by role: with no resolution yet there is no
        // source and no destination. `side_of` reads them back the same way
        // because `target_pane` is 1.
        source: local.map(Side::from_entry),
        dest: remote.map(Side::from_entry),
        included: true,
        resolution: Some(Resolution::Pending),
        caveat: None,
    }
}

/// Whether a path escapes its root once `..` and `.` are resolved.
///
/// Used by the safety check: a sync that writes outside its own root is either
/// a bug or a symlink being followed somewhere it should not be, and either
/// way the user should be asked first.
pub fn escapes_root(root: &Path, relative: &Path) -> bool {
    use std::path::Component;

    let mut depth: i32 = 0;
    for component in relative.components() {
        match component {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Component::CurDir => {}
            // An absolute component inside what should be a relative path
            // would make `root.join(relative)` discard the root entirely.
            Component::RootDir | Component::Prefix(_) => return true,
            Component::Normal(_) => depth += 1,
        }
    }

    let _ = root;
    false
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn entry(name: &str, size: u64, epoch: Option<i64>) -> Entry {
        Entry {
            name: name.into(),
            kind: EntryKind::File,
            size,
            mtime: epoch.map(|s| Utc.timestamp_opt(s, 0).unwrap()),
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            kind: EntryKind::Dir,
            ..entry(name, 0, None)
        }
    }

    fn plan_with(rows: Vec<Row>) -> Plan {
        let mut plan = Plan {
            rows,
            ..Plan::default()
        };
        plan.sort();
        plan
    }

    #[test]
    fn a_new_file_is_a_create_carrying_only_a_source_side() {
        let row = create_row(
            "a.txt".into(),
            &entry("a.txt", 10, Some(1)),
            Method::SizeMtime,
            1,
        );
        assert_eq!(row.action, Action::Create);
        assert_eq!(row.source.unwrap().size, 10);
        assert!(row.dest.is_none());
        assert_eq!(row.bytes(), 10);
        assert_eq!(row.size_delta(), 10);
    }

    #[test]
    fn a_directory_is_its_own_action_and_needs_no_review() {
        let row = create_row("src".into(), &dir("src"), Method::SizeMtime, 1);
        assert_eq!(row.action, Action::CreateDir);
        assert_eq!(row.bytes(), 0);
        assert!(
            !row.needs_review(),
            "a plan whose review list is mostly mkdir is a plan nobody reads"
        );
    }

    #[test]
    fn an_equal_pair_is_a_skip() {
        let row = compare_row(
            "a".into(),
            &entry("a", 10, Some(1)),
            &entry("a", 10, Some(1)),
            Reason::SameSizeAndTime,
            Method::SizeMtime,
            1,
        );
        assert_eq!(row.action, Action::Skip);
        assert!(!row.needs_review());
        assert_eq!(row.bytes(), 0);
    }

    #[test]
    fn a_delete_carries_only_a_destination_side() {
        let row = delete_row(
            "gone".into(),
            &entry("gone", 50, Some(1)),
            Method::SizeMtime,
            1,
        );
        assert_eq!(row.action, Action::Delete);
        assert!(row.source.is_none());
        assert_eq!(row.bytes(), 0);
        assert_eq!(row.bytes_deleted(), 50);
        assert_eq!(row.size_delta(), -50);
    }

    #[test]
    fn totals_add_up() {
        let plan = plan_with(vec![
            create_row(
                "new".into(),
                &entry("new", 100, Some(1)),
                Method::SizeMtime,
                1,
            ),
            compare_row(
                "changed".into(),
                &entry("changed", 200, Some(2)),
                &entry("changed", 150, Some(1)),
                Reason::SizeDiffers,
                Method::SizeMtime,
                1,
            ),
            delete_row(
                "old".into(),
                &entry("old", 30, Some(1)),
                Method::SizeMtime,
                1,
            ),
            compare_row(
                "same".into(),
                &entry("same", 10, Some(1)),
                &entry("same", 10, Some(1)),
                Reason::SameSizeAndTime,
                Method::SizeMtime,
                1,
            ),
        ]);

        let totals = plan.totals();
        assert_eq!(totals.creates, 1);
        assert_eq!(totals.overwrites, 1);
        assert_eq!(totals.deletes, 1);
        assert_eq!(totals.skipped, 1);
        assert_eq!(totals.operations(), 3, "a skip is not an operation");
        assert_eq!(totals.bytes_out, 300);
        assert_eq!(totals.bytes_deleted, 30);
    }

    #[test]
    fn totals_count_rows_that_clobber_newer_data() {
        let plan = plan_with(vec![compare_row(
            "notes".into(),
            &entry("notes", 10, Some(1000)),
            &entry("notes", 10, Some(2000)),
            Reason::DestinationNewer,
            Method::SizeMtime,
            1,
        )]);

        assert_eq!(plan.totals().clobbers_newer, 1);
        assert!(plan.rows[0].clobbers_newer());
    }

    /// Excluding a row has to change the totals without re-planning, which is
    /// the whole reason rows carry a flag instead of being filtered out.
    #[test]
    fn excluding_a_row_recomputes_the_totals() {
        let mut plan = plan_with(vec![
            create_row("a".into(), &entry("a", 100, Some(1)), Method::SizeMtime, 1),
            create_row("b".into(), &entry("b", 50, Some(1)), Method::SizeMtime, 1),
        ]);
        assert_eq!(plan.totals().bytes_out, 150);

        plan.toggle(0);
        assert_eq!(plan.totals().bytes_out, 50);
        assert_eq!(plan.totals().creates, 1);
    }

    #[test]
    fn set_all_and_invert_leave_unreviewable_rows_alone() {
        let mut plan = plan_with(vec![
            create_row("dir".into(), &dir("dir"), Method::SizeMtime, 1),
            create_row("a".into(), &entry("a", 1, Some(1)), Method::SizeMtime, 1),
        ]);

        plan.set_all(false);
        let directory = plan
            .rows
            .iter()
            .find(|r| r.action == Action::CreateDir)
            .unwrap();
        assert!(
            directory.included,
            "excluding every file must not strand the directories they need"
        );
        assert!(!plan.can_apply());

        plan.invert();
        assert!(
            plan.rows
                .iter()
                .find(|r| r.action == Action::Create)
                .unwrap()
                .included
        );
    }

    #[test]
    fn only_includes_a_single_row() {
        let mut plan = plan_with(vec![
            create_row("a".into(), &entry("a", 1, Some(1)), Method::SizeMtime, 1),
            create_row("b".into(), &entry("b", 1, Some(1)), Method::SizeMtime, 1),
            create_row("c".into(), &entry("c", 1, Some(1)), Method::SizeMtime, 1),
        ]);

        plan.only(1);
        assert_eq!(plan.totals().creates, 1);
        assert_eq!(plan.included().filter(|r| r.needs_review()).count(), 1);
        assert_eq!(plan.rows[1].path, Path::new("b"));
    }

    /// Deletions must sort above the skipped list, so the destructive part of
    /// a plan is never hidden below two hundred unchanged files.
    #[test]
    fn sections_sort_with_deletes_before_skips() {
        let plan = plan_with(vec![
            compare_row(
                "z-same".into(),
                &entry("z", 1, Some(1)),
                &entry("z", 1, Some(1)),
                Reason::SameSizeAndTime,
                Method::SizeMtime,
                1,
            ),
            delete_row(
                "a-gone".into(),
                &entry("a", 1, Some(1)),
                Method::SizeMtime,
                1,
            ),
            create_row(
                "m-new".into(),
                &entry("m", 1, Some(1)),
                Method::SizeMtime,
                1,
            ),
        ]);

        let sections: Vec<Section> = plan.rows.iter().map(|r| r.action.section()).collect();
        assert_eq!(
            sections,
            vec![Section::Create, Section::Delete, Section::Skipped]
        );
    }

    #[test]
    fn an_empty_plan_cannot_be_applied() {
        assert!(Plan::default().is_empty());
        assert!(!Plan::default().can_apply());

        // A plan of nothing but skips is also empty for review purposes.
        let plan = plan_with(vec![compare_row(
            "same".into(),
            &entry("same", 1, Some(1)),
            &entry("same", 1, Some(1)),
            Reason::SameSizeAndTime,
            Method::SizeMtime,
            1,
        )]);
        assert!(plan.is_empty());
        assert!(!plan.can_apply());
    }

    #[test]
    fn a_refused_plan_cannot_be_applied_however_many_rows_it_has() {
        let mut plan = plan_with(vec![create_row(
            "a".into(),
            &entry("a", 1, Some(1)),
            Method::SizeMtime,
            1,
        )]);
        assert!(plan.can_apply());

        plan.refusal = Some("too many deletions".into());
        assert!(!plan.can_apply());
    }

    /// Parents must come before children, or a create fails on a missing
    /// directory.
    #[test]
    fn required_directories_are_shallowest_first() {
        let plan = plan_with(vec![create_row(
            "src/api/v2/handler.rs".into(),
            &entry("handler.rs", 1, Some(1)),
            Method::SizeMtime,
            1,
        )]);

        let dirs = plan.required_directories();
        assert_eq!(
            dirs,
            vec![
                (1, PathBuf::from("src")),
                (1, PathBuf::from("src/api")),
                (1, PathBuf::from("src/api/v2")),
            ]
        );
    }

    /// An excluded row must not leave behind a directory created only for it.
    #[test]
    fn required_directories_follow_inclusion() {
        let mut plan = plan_with(vec![create_row(
            "only/here/f".into(),
            &entry("f", 1, Some(1)),
            Method::SizeMtime,
            1,
        )]);
        assert_eq!(plan.required_directories().len(), 2);

        plan.set_all(false);
        assert!(
            plan.required_directories().is_empty(),
            "excluding the only file in a directory should not create the directory"
        );
    }

    #[test]
    fn a_delete_needs_no_directories() {
        let plan = plan_with(vec![delete_row(
            "a/b/c".into(),
            &entry("c", 1, Some(1)),
            Method::SizeMtime,
            1,
        )]);
        assert!(plan.required_directories().is_empty());
    }

    /// The guard against writing outside the sync root.
    #[test]
    fn paths_that_escape_the_root_are_detected() {
        let root = Path::new("/srv/www");

        assert!(!escapes_root(root, Path::new("src/api/handler.rs")));
        assert!(!escapes_root(root, Path::new("a/../b")));
        assert!(!escapes_root(root, Path::new("./a")));

        assert!(escapes_root(root, Path::new("../outside")));
        assert!(escapes_root(root, Path::new("a/../../outside")));
        assert!(
            escapes_root(root, Path::new("/etc/passwd")),
            "an absolute component would discard the root in a join"
        );
    }

    #[test]
    fn section_titles_exist_and_destructive_ones_are_marked() {
        for section in Section::ALL {
            assert!(!section.title().is_empty());
        }
        assert!(Section::Delete.is_destructive());
        assert!(Section::Overwrite.is_destructive());
        assert!(!Section::Create.is_destructive());
        assert!(!Section::Skipped.is_destructive());
    }

    /// A plan over a huge tree must not overflow its byte counters.
    #[test]
    fn totals_do_not_overflow_on_large_files() {
        let plan = plan_with(vec![
            create_row(
                "a".into(),
                &entry("a", u64::MAX / 2, Some(1)),
                Method::SizeMtime,
                1,
            ),
            create_row(
                "b".into(),
                &entry("b", u64::MAX / 2, Some(1)),
                Method::SizeMtime,
                1,
            ),
        ]);
        // Saturating rather than wrapping would be wrong too, but panicking in
        // a debug build while drawing a frame is the failure to avoid.
        let _ = plan.totals().bytes_out;
    }
}
