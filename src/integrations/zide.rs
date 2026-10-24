//! Opening the right bookmark when launched from zide.
//!
//! lazysync is meant to be popped up as a zide floating pane the way `lazygit`
//! is. In that situation it already knows something useful: the directory the
//! pane started in. If that directory is one a bookmark points at, showing a
//! list and making you pick is a wasted keystroke.
//!
//! The matching is **exact first, then nearest ancestor**. The ancestor case
//! is what makes it useful rather than merely convenient: popping the panel
//! from `~/proj/src/api` should find the `~/proj` bookmark *and* land the
//! remote pane on `src/api`, because that is where you were working. Matching
//! only exact paths would mean the feature fires roughly never.
//!
//! Deepest wins when several bookmarks match, so a bookmark for `~/proj/docs`
//! beats one for `~/proj` when you are inside `docs`.

use std::path::{Path, PathBuf};

use crate::{
    bookmarks::{Bookmark, BookmarkStore},
    config::ZideMatch,
};

/// A bookmark chosen for the working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Index into the store's bookmark list.
    pub index: usize,
    /// How far inside the bookmark's `local_path` the working directory is.
    ///
    /// Empty for an exact match. Otherwise this is the subpath to open in
    /// *both* panes, which is what makes the popup land where you were.
    pub relative: PathBuf,
}

impl Match {
    pub fn is_exact(&self) -> bool {
        // An empty relative path means the working directory *is* the
        // bookmark's local path.
        self.relative.as_os_str().is_empty()
    }
}

/// Decide which bookmark, if any, the working directory selects.
///
/// Returns every candidate, deepest first. One result is the interesting case;
/// more than one means equally deep matches, which the caller should present
/// as a filtered list rather than guessing between.
pub fn resolve(store: &BookmarkStore, cwd: &Path, policy: ZideMatch) -> Vec<Match> {
    if matches!(policy, ZideMatch::Off) {
        return Vec::new();
    }

    let mut matches: Vec<(usize, Match)> = Vec::new();

    for (index, bookmark) in store.bookmarks.iter().enumerate() {
        let Some(local) = &bookmark.local_path else {
            continue;
        };

        // Compare lexically normalised paths. Without this, a `local_path` of
        // `~/proj/` with a trailing separator, or one containing `..`, would
        // fail to match the very directory it names.
        let local = normalize(local);
        let here = normalize(cwd);

        let Ok(relative) = here.strip_prefix(&local) else {
            continue;
        };

        // `Path::strip_prefix` compares whole components rather than bytes,
        // which is what keeps `/home/greg/projects` from matching a bookmark
        // for `/home/greg/proj`. A `starts_with` on the string forms would get
        // that wrong, hence paths rather than strings throughout.
        let depth = local.components().count();

        if !relative.as_os_str().is_empty() && matches!(policy, ZideMatch::Exact) {
            continue;
        }

        matches.push((
            depth,
            Match {
                index,
                relative: relative.to_path_buf(),
            },
        ));
    }

    // Deepest local_path first: a bookmark for ~/proj/docs should beat one for
    // ~/proj when the working directory is inside docs.
    matches.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));

    let deepest = matches.first().map(|(depth, _)| *depth);
    matches
        .into_iter()
        .filter(|(depth, _)| Some(*depth) == deepest)
        .map(|(_, m)| m)
        .collect()
}

/// The single bookmark to open, if the working directory picks exactly one.
pub fn single<'a>(
    store: &'a BookmarkStore,
    cwd: &Path,
    policy: ZideMatch,
) -> Option<(&'a Bookmark, Match)> {
    let mut matches = resolve(store, cwd, policy);
    if matches.len() != 1 {
        return None;
    }
    let found = matches.remove(0);
    store.bookmarks.get(found.index).map(|b| (b, found))
}

/// Resolve `..` and `.` and drop a trailing separator, without touching disk.
///
/// Lexical on purpose, matching `LocalFs::normalize`: a bookmark whose
/// `local_path` goes through a symlink should still match the path the user
/// actually typed, and resolving links would make the two disagree.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether lazysync appears to have been launched from inside zide.
///
/// Only used for a log line and for the status hint: the matching itself keys
/// off the working directory, which is set the same way whether zide or a
/// plain shell started the process. Making the feature *depend* on detecting
/// zide would break it for anyone launching lazysync from a wrapper script.
pub fn launched_from_zide() -> bool {
    std::env::var_os("ZIDE_DIR").is_some() || std::env::var_os("ZELLIJ").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{Protocol, Target};

    fn bookmark(name: &str, local: Option<&str>) -> Bookmark {
        let mut b = Bookmark::from_target(name, &Target::new(Protocol::Sftp, "box"));
        b.local_path = local.map(PathBuf::from);
        b
    }

    fn store(entries: &[(&str, Option<&str>)]) -> BookmarkStore {
        let mut store = BookmarkStore::default();
        for (name, local) in entries {
            store.upsert(bookmark(name, *local));
        }
        store
    }

    #[test]
    fn an_exact_match_is_found() {
        let store = store(&[("proj", Some("/home/greg/proj"))]);
        let found = resolve(&store, Path::new("/home/greg/proj"), ZideMatch::Ancestor);

        assert_eq!(found.len(), 1);
        assert!(found[0].is_exact());
        assert_eq!(found[0].relative, Path::new(""));
    }

    /// The case that makes the feature worth having: the popup opens from a
    /// subdirectory, and both panes should land there.
    #[test]
    fn an_ancestor_match_carries_the_subpath() {
        let store = store(&[("proj", Some("/home/greg/proj"))]);
        let found = resolve(
            &store,
            Path::new("/home/greg/proj/src/api"),
            ZideMatch::Ancestor,
        );

        assert_eq!(found.len(), 1);
        assert!(!found[0].is_exact());
        assert_eq!(found[0].relative, Path::new("src/api"));
    }

    #[test]
    fn exact_policy_ignores_ancestors() {
        let store = store(&[("proj", Some("/home/greg/proj"))]);

        assert!(resolve(&store, Path::new("/home/greg/proj/src"), ZideMatch::Exact).is_empty());
        assert_eq!(
            resolve(&store, Path::new("/home/greg/proj"), ZideMatch::Exact).len(),
            1
        );
    }

    #[test]
    fn off_matches_nothing() {
        let store = store(&[("proj", Some("/home/greg/proj"))]);
        assert!(resolve(&store, Path::new("/home/greg/proj"), ZideMatch::Off).is_empty());
    }

    /// A bookmark for a deeper directory should win, or the specific one you
    /// made for `docs` would never fire.
    #[test]
    fn the_deepest_local_path_wins() {
        let store = store(&[
            ("proj", Some("/home/greg/proj")),
            ("docs", Some("/home/greg/proj/docs")),
        ]);

        let found = resolve(
            &store,
            Path::new("/home/greg/proj/docs/api"),
            ZideMatch::Ancestor,
        );

        assert_eq!(found.len(), 1);
        assert_eq!(store.bookmarks[found[0].index].name, "docs");
        assert_eq!(found[0].relative, Path::new("api"));
    }

    /// Equally deep matches are ambiguous, and the caller is expected to show
    /// a filtered list rather than pick one arbitrarily.
    #[test]
    fn equally_deep_matches_are_all_returned() {
        let store = store(&[
            ("a", Some("/home/greg/proj")),
            ("b", Some("/home/greg/proj")),
        ]);

        let found = resolve(
            &store,
            Path::new("/home/greg/proj/src"),
            ZideMatch::Ancestor,
        );
        assert_eq!(found.len(), 2);
        assert!(single(
            &store,
            Path::new("/home/greg/proj/src"),
            ZideMatch::Ancestor
        )
        .is_none());
    }

    /// `strip_prefix` is textual, so this is the trap: `/home/greg/projects`
    /// must not match a bookmark for `/home/greg/proj`.
    #[test]
    fn a_sibling_directory_with_a_shared_prefix_does_not_match() {
        let store = store(&[("proj", Some("/home/greg/proj"))]);
        assert!(resolve(
            &store,
            Path::new("/home/greg/projects"),
            ZideMatch::Ancestor
        )
        .is_empty());
        assert!(resolve(
            &store,
            Path::new("/home/greg/proj-old/src"),
            ZideMatch::Ancestor
        )
        .is_empty());
    }

    #[test]
    fn a_trailing_separator_in_the_bookmark_still_matches() {
        let store = store(&[("proj", Some("/home/greg/proj/"))]);
        let found = resolve(
            &store,
            Path::new("/home/greg/proj/src"),
            ZideMatch::Ancestor,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].relative, Path::new("src"));
    }

    #[test]
    fn dot_dot_in_either_path_is_resolved() {
        let store = store(&[("proj", Some("/home/greg/other/../proj"))]);
        let found = resolve(
            &store,
            Path::new("/home/greg/proj/src/../src/api"),
            ZideMatch::Ancestor,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].relative, Path::new("src/api"));
    }

    #[test]
    fn bookmarks_without_a_local_path_are_skipped() {
        let store = store(&[("remote-only", None), ("proj", Some("/home/greg/proj"))]);
        let found = resolve(&store, Path::new("/home/greg/proj"), ZideMatch::Ancestor);

        assert_eq!(found.len(), 1);
        assert_eq!(store.bookmarks[found[0].index].name, "proj");
    }

    #[test]
    fn an_unrelated_directory_matches_nothing() {
        let store = store(&[("proj", Some("/home/greg/proj"))]);
        assert!(resolve(&store, Path::new("/tmp"), ZideMatch::Ancestor).is_empty());
        assert!(single(&store, Path::new("/tmp"), ZideMatch::Ancestor).is_none());
    }

    #[test]
    fn single_returns_the_bookmark_and_its_subpath() {
        let store = store(&[("proj", Some("/home/greg/proj"))]);
        let (bookmark, found) = single(
            &store,
            Path::new("/home/greg/proj/src"),
            ZideMatch::Ancestor,
        )
        .unwrap();

        assert_eq!(bookmark.name, "proj");
        assert_eq!(found.relative, Path::new("src"));
    }
}
