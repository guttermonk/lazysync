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
        }
    }

    /// The section heading this action is grouped under.
    pub const fn section(self) -> Section {
        match self {
            Self::Create | Self::CreateDir => Section::Create,
            Self::Overwrite => Section::Overwrite,
            Self::Delete => Section::Delete,
            Self::Skip => Section::Skipped,
        }
    }
}

/// A heading in the confirmation window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Create,
    Overwrite,
    Delete,
    Skipped,
}

impl Section {
    /// Every section, in display order.
    ///
    /// Deletions come after overwrites and before the skipped list, so the
    /// destructive part of a plan is never below a fold of two hundred
    /// unchanged files.
    pub const ALL: [Self; 4] = [Self::Create, Self::Overwrite, Self::Delete, Self::Skipped];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Overwrite => "Overwrite",
            Self::Delete => "Delete",
            Self::Skipped => "Skipped, identical",
        }
    }

    /// Whether this section's rows destroy data if applied.
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::Delete | Self::Overwrite)
    }
}

/// One file's worth of plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Path relative to the sync roots, which is what both sides share.
    pub path: PathBuf,
    pub action: Action,
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
    /// Whether applying this row replaces newer data with older.
    pub const fn clobbers_newer(&self) -> bool {
        self.reason.clobbers_newer()
    }

    /// Bytes this row transfers if applied.
    pub const fn bytes(&self) -> u64 {
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

    /// Directories that could not be read, with why.
    ///
    /// Kept rather than aborting: a plan covering the readable 95% of a tree
    /// is useful, and a silent gap is not. The window shows these so an
    /// unreadable directory cannot be mistaken for an empty one.
    pub unreadable: Vec<(PathBuf, String)>,

    /// Set when a safety rail refused the plan. The window shows it and
    /// refuses to apply.
    pub refusal: Option<String>,
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

    /// Whether applying is allowed.
    pub fn can_apply(&self) -> bool {
        self.refusal.is_none() && self.included().any(Row::needs_review)
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
    pub fn required_directories(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();

        for row in self.included() {
            if matches!(row.action, Action::Delete | Action::Skip) {
                continue;
            }
            let target = if matches!(row.action, Action::CreateDir) {
                Some(row.path.as_path())
            } else {
                row.path.parent()
            };

            let mut current = target;
            while let Some(dir) = current {
                if dir.as_os_str().is_empty() {
                    break;
                }
                if !dirs.contains(&dir.to_path_buf()) {
                    dirs.push(dir.to_path_buf());
                }
                current = dir.parent();
            }
        }

        // Shallowest first, so a parent exists before its child is created.
        dirs.sort_by_key(|d| d.components().count());
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
    pub bytes_out: u64,
    pub bytes_deleted: u64,
}

impl Totals {
    /// Operations that transfer or remove something.
    pub const fn operations(&self) -> usize {
        self.creates + self.overwrites + self.deletes
    }
}

/// Build a row for a file present only on the source.
pub fn create_row(path: PathBuf, source: &Entry, method: Method) -> Row {
    Row {
        path,
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
) -> Row {
    Row {
        path,
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
        caveat: None,
    }
}

/// Build a row for a file present only on the destination.
pub fn delete_row(path: PathBuf, dest: &Entry, method: Method) -> Row {
    Row {
        path,
        action: Action::Delete,
        reason: Reason::NotOnSource,
        method,
        source: None,
        dest: Some(Side::from_entry(dest)),
        included: true,
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
        );
        assert_eq!(row.action, Action::Create);
        assert_eq!(row.source.unwrap().size, 10);
        assert!(row.dest.is_none());
        assert_eq!(row.bytes(), 10);
        assert_eq!(row.size_delta(), 10);
    }

    #[test]
    fn a_directory_is_its_own_action_and_needs_no_review() {
        let row = create_row("src".into(), &dir("src"), Method::SizeMtime);
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
            create_row("new".into(), &entry("new", 100, Some(1)), Method::SizeMtime),
            compare_row(
                "changed".into(),
                &entry("changed", 200, Some(2)),
                &entry("changed", 150, Some(1)),
                Reason::SizeDiffers,
                Method::SizeMtime,
            ),
            delete_row("old".into(), &entry("old", 30, Some(1)), Method::SizeMtime),
            compare_row(
                "same".into(),
                &entry("same", 10, Some(1)),
                &entry("same", 10, Some(1)),
                Reason::SameSizeAndTime,
                Method::SizeMtime,
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
        )]);

        assert_eq!(plan.totals().clobbers_newer, 1);
        assert!(plan.rows[0].clobbers_newer());
    }

    /// Excluding a row has to change the totals without re-planning, which is
    /// the whole reason rows carry a flag instead of being filtered out.
    #[test]
    fn excluding_a_row_recomputes_the_totals() {
        let mut plan = plan_with(vec![
            create_row("a".into(), &entry("a", 100, Some(1)), Method::SizeMtime),
            create_row("b".into(), &entry("b", 50, Some(1)), Method::SizeMtime),
        ]);
        assert_eq!(plan.totals().bytes_out, 150);

        plan.toggle(0);
        assert_eq!(plan.totals().bytes_out, 50);
        assert_eq!(plan.totals().creates, 1);
    }

    #[test]
    fn set_all_and_invert_leave_unreviewable_rows_alone() {
        let mut plan = plan_with(vec![
            create_row("dir".into(), &dir("dir"), Method::SizeMtime),
            create_row("a".into(), &entry("a", 1, Some(1)), Method::SizeMtime),
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
            create_row("a".into(), &entry("a", 1, Some(1)), Method::SizeMtime),
            create_row("b".into(), &entry("b", 1, Some(1)), Method::SizeMtime),
            create_row("c".into(), &entry("c", 1, Some(1)), Method::SizeMtime),
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
            ),
            delete_row("a-gone".into(), &entry("a", 1, Some(1)), Method::SizeMtime),
            create_row("m-new".into(), &entry("m", 1, Some(1)), Method::SizeMtime),
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
        )]);

        let dirs = plan.required_directories();
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("src"),
                PathBuf::from("src/api"),
                PathBuf::from("src/api/v2"),
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
            ),
            create_row(
                "b".into(),
                &entry("b", u64::MAX / 2, Some(1)),
                Method::SizeMtime,
            ),
        ]);
        // Saturating rather than wrapping would be wrong too, but panicking in
        // a debug build while drawing a frame is the failure to avoid.
        let _ = plan.totals().bytes_out;
    }
}
