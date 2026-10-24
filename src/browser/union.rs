//! Union view: both panes' listings merged into one set of rows.
//!
//! Synchronized browsing couples the two *directories*; it does not pretend
//! the two listings are the same. So a sparse local checkout against a full
//! remote shows three entries on the left and four hundred on the right, and
//! the entries that only exist remotely are simply not reachable from the
//! left pane.
//!
//! Union view fixes that by merging the listings and marking where each row
//! came from. Entering a row that exists on only one side moves that pane and
//! ghosts the other, which is exactly the behaviour synchronized browsing
//! already has -- union view just makes those rows selectable in the first
//! place.

use std::collections::HashMap;

use crate::{config::SortSpec, vfs::Entry};

/// Which side(s) a row exists on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Present in both panes.
    Both,
    LocalOnly,
    RemoteOnly,
}

impl Origin {
    /// The marker shown in the row: `LR`, `L ` or ` R`.
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Both => "LR",
            Self::LocalOnly => "L ",
            Self::RemoteOnly => " R",
        }
    }

    pub const fn is_one_sided(self) -> bool {
        !matches!(self, Self::Both)
    }
}

/// One merged row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionRow {
    pub name: String,
    pub local: Option<Entry>,
    pub remote: Option<Entry>,
}

impl UnionRow {
    pub const fn origin(&self) -> Origin {
        match (self.local.is_some(), self.remote.is_some()) {
            (true, true) => Origin::Both,
            (true, false) => Origin::LocalOnly,
            (false, true) => Origin::RemoteOnly,
            // Rows are only built from at least one entry, so this is
            // unreachable; treating it as Both keeps the function total
            // without an unwrap.
            (false, false) => Origin::Both,
        }
    }

    /// This row's entry on one side.
    pub fn side(&self, index: usize) -> Option<&Entry> {
        if index == 0 {
            self.local.as_ref()
        } else {
            self.remote.as_ref()
        }
    }

    /// Whether entering this row navigates into it on either side.
    pub fn is_dir_like(&self) -> bool {
        self.local.as_ref().is_some_and(Entry::is_dir_like)
            || self.remote.as_ref().is_some_and(Entry::is_dir_like)
    }

    /// The entry to display attributes from: the side it exists on, or the
    /// local one when it exists on both.
    pub fn representative(&self) -> Option<&Entry> {
        self.local.as_ref().or(self.remote.as_ref())
    }
}

/// Merge two listings by name.
///
/// Both inputs are expected to be already filtered for hidden entries, so a
/// dotfile does not reappear from the other side's listing when it is hidden
/// on this one.
pub fn merge(local: &[Entry], remote: &[Entry], sort: SortSpec, dirs_first: bool) -> Vec<UnionRow> {
    let mut rows: Vec<UnionRow> = Vec::with_capacity(local.len().max(remote.len()));

    for entry in local {
        rows.push(UnionRow {
            name: entry.name.clone(),
            local: Some(entry.clone()),
            remote: None,
        });
    }

    // Index by name rather than scanning: a directory with thousands of
    // entries would otherwise make this quadratic, and remote listings get
    // that large.
    let mut index: HashMap<&str, usize> = local
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.name.as_str(), i))
        .collect();

    for entry in remote {
        // Case-sensitive matching: a remote that is case-insensitive would
        // need its own comparison, and silently folding `README` into
        // `readme` here would hide a genuine difference between the trees.
        match index.get(entry.name.as_str()) {
            Some(&i) => rows[i].remote = Some(entry.clone()),
            None => {
                index.insert(entry.name.as_str(), rows.len());
                rows.push(UnionRow {
                    name: entry.name.clone(),
                    local: None,
                    remote: Some(entry.clone()),
                });
            }
        }
    }

    sort_rows(&mut rows, sort, dirs_first);
    rows
}

fn sort_rows(rows: &mut [UnionRow], sort: SortSpec, dirs_first: bool) {
    // Sorting goes through the same comparator as a single pane, by ordering a
    // throwaway entry list and then applying that order to the rows. Sharing
    // the comparator is the point: a merged view that ordered things
    // differently from the panes it was built from would be disorienting.
    //
    // A row's representative is the local entry where it exists on both
    // sides, so `dirs_first` follows whichever side is present -- a
    // remote-only directory still sorts with the directories.
    let mut entries: Vec<Entry> = rows
        .iter()
        .filter_map(|row| row.representative().cloned())
        .collect();
    super::sort::sort(&mut entries, sort, dirs_first);

    let rank: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.name.as_str(), i))
        .collect();
    rows.sort_by_key(|row| rank.get(row.name.as_str()).copied().unwrap_or(usize::MAX));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::EntryKind;

    fn file(name: &str) -> Entry {
        Entry {
            name: name.into(),
            kind: EntryKind::File,
            size: 0,
            mtime: None,
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            kind: EntryKind::Dir,
            ..file(name)
        }
    }

    fn names(rows: &[UnionRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    /// The asymmetric case: a sparse local tree against a full remote.
    #[test]
    fn one_sided_entries_appear_and_are_marked() {
        let local = [dir("src"), file("README.md")];
        let remote = [dir("src"), dir("docs"), file("deploy.sh")];

        let rows = merge(&local, &remote, SortSpec::default(), true);

        assert_eq!(rows.len(), 4);
        let by_name = |name: &str| rows.iter().find(|r| r.name == name).unwrap().origin();
        assert_eq!(by_name("src"), Origin::Both);
        assert_eq!(by_name("README.md"), Origin::LocalOnly);
        assert_eq!(by_name("docs"), Origin::RemoteOnly);
        assert_eq!(by_name("deploy.sh"), Origin::RemoteOnly);
    }

    #[test]
    fn directories_sort_first_across_the_merged_set() {
        let local = [file("a.txt")];
        let remote = [dir("zzz")];

        let rows = merge(&local, &remote, SortSpec::default(), true);
        assert_eq!(names(&rows), ["zzz", "a.txt"]);
    }

    #[test]
    fn a_row_present_on_both_sides_keeps_both_entries() {
        let mut local_entry = file("notes.md");
        local_entry.size = 10;
        let mut remote_entry = file("notes.md");
        remote_entry.size = 20;

        let rows = merge(&[local_entry], &[remote_entry], SortSpec::default(), true);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].local.as_ref().unwrap().size, 10);
        assert_eq!(rows[0].remote.as_ref().unwrap().size, 20);
        assert_eq!(rows[0].side(0).unwrap().size, 10);
        assert_eq!(rows[0].side(1).unwrap().size, 20);
    }

    #[test]
    fn a_remote_only_directory_is_enterable() {
        let rows = merge(&[], &[dir("web")], SortSpec::default(), true);
        assert!(rows[0].is_dir_like());
        assert_eq!(rows[0].origin(), Origin::RemoteOnly);
    }

    /// Names differing only in case are two different entries, because on a
    /// case-sensitive remote they are.
    #[test]
    fn case_differences_are_not_folded_together() {
        let rows = merge(
            &[file("README")],
            &[file("readme")],
            SortSpec::default(),
            true,
        );
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn merging_two_empty_listings_gives_nothing() {
        assert!(merge(&[], &[], SortSpec::default(), true).is_empty());
    }

    #[test]
    fn markers_distinguish_the_three_cases() {
        assert_eq!(Origin::Both.marker(), "LR");
        assert_eq!(Origin::LocalOnly.marker(), "L ");
        assert_eq!(Origin::RemoteOnly.marker(), " R");
        assert!(!Origin::Both.is_one_sided());
        assert!(Origin::LocalOnly.is_one_sided());
    }
}
