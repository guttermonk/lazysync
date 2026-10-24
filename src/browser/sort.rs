//! Ordering and filtering a listing.

use crate::{
    config::{SortKey, SortSpec},
    vfs::Entry,
};

/// Order `entries` in place.
///
/// `dirs_first` is applied before the sort key rather than as part of it, so
/// sorting by size does not scatter directories through the file list.
pub fn sort(entries: &mut [Entry], spec: SortSpec, dirs_first: bool) {
    entries.sort_by(|a, b| {
        if dirs_first {
            let ordering = b.is_dir_like().cmp(&a.is_dir_like());
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }

        let ordering = match spec.key {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Size => a.size.cmp(&b.size),
            // Entries with no timestamp sort together at the start rather than
            // being spread arbitrarily -- S3 listings are all-or-nothing here.
            SortKey::Modified => a.mtime.cmp(&b.mtime),
            SortKey::Extension => extension(&a.name)
                .cmp(extension(&b.name))
                .then_with(|| natural_cmp(&a.name, &b.name)),
        };

        if spec.reverse {
            ordering.reverse()
        } else {
            ordering
        }
    });
}

/// Drop entries the current view hides.
pub fn filter(entries: &[Entry], show_hidden: bool) -> Vec<Entry> {
    entries
        .iter()
        .filter(|entry| show_hidden || !entry.is_hidden())
        .cloned()
        .collect()
}

fn extension(name: &str) -> &str {
    // rsplit_once rather than Path::extension so that a dotfile with no
    // extension (".bashrc") does not sort as though "bashrc" were one.
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext,
        _ => "",
    }
}

/// Case-insensitive comparison that orders embedded numbers by value.
///
/// Without this, `part2` sorts after `part10`, which in a directory of
/// numbered files is just wrong.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut a_chars = a.chars().peekable();
    let mut b_chars = b.chars().peekable();

    loop {
        match (a_chars.peek().copied(), b_chars.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let a_num = take_number(&mut a_chars);
                    let b_num = take_number(&mut b_chars);
                    match a_num.cmp(&b_num) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                }

                a_chars.next();
                b_chars.next();

                // Fold case for ordering, but fall back to the raw characters
                // so that "A" and "a" are still distinguishable and the sort
                // stays total.
                let folded = ac
                    .to_lowercase()
                    .cmp(bc.to_lowercase())
                    .then_with(|| ac.cmp(&bc));
                if folded != Ordering::Equal {
                    return folded;
                }
            }
        }
    }
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u128 {
    let mut value: u128 = 0;
    while let Some(c) = chars.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        chars.next();
        // Saturate rather than wrap: a 40-digit "number" in a filename should
        // sort as very large, not as whatever it overflows to.
        value = value
            .saturating_mul(10)
            .saturating_add(u128::from(c as u8 - b'0'));
    }
    value
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::vfs::EntryKind;

    fn file(name: &str, size: u64) -> Entry {
        Entry {
            name: name.into(),
            kind: EntryKind::File,
            size,
            mtime: None,
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }

    fn dir(name: &str) -> Entry {
        Entry {
            kind: EntryKind::Dir,
            ..file(name, 0)
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn directories_come_first_whatever_the_sort_key() {
        let mut entries = vec![file("a.txt", 9000), dir("zzz"), file("b.txt", 1)];
        sort(
            &mut entries,
            SortSpec {
                key: SortKey::Size,
                reverse: false,
            },
            true,
        );
        assert_eq!(names(&entries), ["zzz", "b.txt", "a.txt"]);
    }

    #[test]
    fn numbers_in_names_sort_by_value() {
        let mut entries = vec![file("part10", 0), file("part2", 0), file("part1", 0)];
        sort(&mut entries, SortSpec::default(), false);
        assert_eq!(names(&entries), ["part1", "part2", "part10"]);
    }

    #[test]
    fn name_sort_is_case_insensitive() {
        let mut entries = vec![file("Beta", 0), file("alpha", 0), file("Gamma", 0)];
        sort(&mut entries, SortSpec::default(), false);
        assert_eq!(names(&entries), ["alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn reverse_flips_the_order() {
        let mut entries = vec![file("a", 0), file("b", 0)];
        sort(
            &mut entries,
            SortSpec {
                key: SortKey::Name,
                reverse: true,
            },
            false,
        );
        assert_eq!(names(&entries), ["b", "a"]);
    }

    /// A backend with no timestamps must not produce an arbitrary order.
    #[test]
    fn entries_without_a_timestamp_group_together() {
        let mut stamped = file("stamped", 0);
        stamped.mtime = Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());

        let mut entries = vec![stamped, file("unstamped", 0)];
        sort(
            &mut entries,
            SortSpec {
                key: SortKey::Modified,
                reverse: false,
            },
            false,
        );
        assert_eq!(names(&entries), ["unstamped", "stamped"]);
    }

    #[test]
    fn a_dotfile_has_no_extension() {
        assert_eq!(extension(".bashrc"), "");
        assert_eq!(extension("notes.md"), "md");
        assert_eq!(extension("archive.tar.gz"), "gz");
        assert_eq!(extension("Makefile"), "");
    }

    #[test]
    fn hidden_entries_are_filtered_unless_asked_for() {
        let entries = vec![file(".hidden", 0), file("visible", 0)];
        assert_eq!(names(&filter(&entries, false)), ["visible"]);
        assert_eq!(names(&filter(&entries, true)), [".hidden", "visible"]);
    }

    /// A very long digit run must not panic or wrap.
    #[test]
    fn absurd_numbers_saturate_rather_than_overflow() {
        let huge = "9".repeat(60);
        let mut entries = vec![file(&huge, 0), file("1", 0)];
        sort(&mut entries, SortSpec::default(), false);
        assert_eq!(names(&entries), ["1", huge.as_str()]);
    }
}
