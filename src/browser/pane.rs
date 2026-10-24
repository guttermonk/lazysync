//! One side of the browser.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crate::{
    config::{Config, SortKey, SortSpec},
    vfs::{Entry, Vfs, VfsError},
};

/// What a pane is currently showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneState {
    /// A real directory listing.
    Listed,
    /// Synchronized browsing followed the other pane to a path that does not
    /// exist here.
    ///
    /// The pane keeps the virtual path it is standing on, which is the whole
    /// point: when the leader navigates back up, the virtual path shortens
    /// until it names a directory that does exist and the panes re-couple.
    /// Holding a real path instead is what makes termscp's version desync for
    /// good.
    Ghost,
    /// The listing failed for a reason worth reporting.
    Failed(String),
}

/// One side of the dual-pane browser.
pub struct Pane {
    pub vfs: Box<dyn Vfs>,
    /// The deepest real directory this pane has listed.
    cwd: PathBuf,
    /// Where the pane is pretending to be, when that is not a real directory.
    ghost: Option<PathBuf>,
    pub state: PaneState,

    /// Everything the backend returned, before filtering or sorting.
    all: Vec<Entry>,
    /// What is on screen, in display order.
    view: Vec<Entry>,

    cursor: usize,
    /// First visible row, tracked here rather than in the widget so that
    /// scrolling survives a re-render.
    pub scroll: usize,

    /// Marked entries, by name within the current directory.
    marks: HashSet<String>,

    /// The anchor synchronized browsing measures relative paths from.
    pub sync_root: PathBuf,

    pub show_hidden: bool,
    pub sort: SortSpec,
    pub dirs_first: bool,
    /// Last search query, for `n`/`N`.
    pub search: Option<String>,
}

impl Pane {
    /// Open `vfs` at `start`, listing it immediately.
    pub fn new(mut vfs: Box<dyn Vfs>, start: PathBuf, config: &Config) -> Self {
        // An unreadable start directory should not be fatal: fall back to the
        // root, which every backend has, rather than refusing to open.
        let cwd = if vfs.is_dir(&start) {
            start
        } else {
            tracing::warn!("{} is not a directory, starting at root", start.display());
            vfs.root()
        };

        let mut pane = Self {
            vfs,
            sync_root: cwd.clone(),
            cwd,
            ghost: None,
            state: PaneState::Listed,
            all: Vec::new(),
            view: Vec::new(),
            cursor: 0,
            scroll: 0,
            marks: HashSet::new(),
            show_hidden: config.show_hidden,
            sort: config.sort,
            dirs_first: config.dirs_first,
            search: None,
        };
        pane.reload();
        pane
    }

    // ── Where we are ─────────────────────────────────────────────────────

    /// The path shown in the pane title: the ghost path when ghosted.
    pub fn path(&self) -> &Path {
        self.ghost.as_deref().unwrap_or(&self.cwd)
    }

    /// The last real directory, regardless of ghosting.
    pub fn real_path(&self) -> &Path {
        &self.cwd
    }

    pub const fn is_ghost(&self) -> bool {
        self.ghost.is_some()
    }

    /// This pane's position relative to its synchronized-browsing anchor.
    ///
    /// `None` when the pane has navigated outside its anchor, which is what
    /// re-anchors synchronized browsing rather than producing nonsense
    /// relative paths full of `..`.
    pub fn relative_to_sync_root(&self) -> Option<PathBuf> {
        self.path()
            .strip_prefix(&self.sync_root)
            .ok()
            .map(Path::to_path_buf)
    }

    /// Re-anchor synchronized browsing here.
    pub fn reanchor(&mut self) {
        self.sync_root = self.path().to_path_buf();
    }

    // ── Listing ──────────────────────────────────────────────────────────

    /// Re-read the current directory.
    pub fn reload(&mut self) {
        if self.ghost.is_some() {
            // Nothing to read; a ghost pane deliberately shows an empty view.
            self.all.clear();
            self.rebuild_view();
            return;
        }

        match self.vfs.list(&self.cwd) {
            Ok(entries) => {
                self.all = entries;
                self.state = PaneState::Listed;
            }
            Err(e) => {
                self.all.clear();
                self.state = PaneState::Failed(e.to_string());
            }
        }
        self.rebuild_view();
    }

    /// Re-apply filtering and sorting, keeping the cursor on the same entry
    /// where possible.
    ///
    /// Preserving the selection by *name* rather than by index is what stops
    /// toggling dotfiles or changing the sort key from moving the cursor onto
    /// an unrelated file -- which, with a delete binding one keystroke away,
    /// matters more than the convenience.
    pub fn rebuild_view(&mut self) {
        let selected_name = self.selected().map(|e| e.name.clone());

        self.view = super::sort::filter(&self.all, self.show_hidden);
        super::sort::sort(&mut self.view, self.sort, self.dirs_first);

        self.cursor = selected_name
            .and_then(|name| self.view.iter().position(|e| e.name == name))
            .unwrap_or(0);
        self.clamp_cursor();
    }

    pub fn entries(&self) -> &[Entry] {
        &self.view
    }

    pub fn len(&self) -> usize {
        self.view.len()
    }

    pub fn is_empty(&self) -> bool {
        self.view.is_empty()
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.view.get(self.cursor)
    }

    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    // ── Navigation ───────────────────────────────────────────────────────

    /// Go to a real directory, clearing any ghost state.
    pub fn navigate_to(&mut self, path: PathBuf) {
        self.cwd = path;
        self.ghost = None;
        self.marks.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.reload();
    }

    /// Stand on a path that does not exist on this side.
    pub fn ghost_at(&mut self, path: PathBuf) {
        self.ghost = Some(path);
        self.state = PaneState::Ghost;
        self.marks.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.all.clear();
        self.rebuild_view();
    }

    /// Enter the selected entry, if it is a directory.
    ///
    /// Returns the path entered, which synchronized browsing uses to move the
    /// other pane.
    pub fn enter_selected(&mut self) -> Option<PathBuf> {
        let entry = self.selected()?;
        if !entry.is_dir_like() {
            return None;
        }
        let target = self.path().join(&entry.name);
        self.navigate_to(target.clone());
        Some(target)
    }

    /// Go up one level. Returns the new path, or `None` at the root.
    pub fn parent(&mut self) -> Option<PathBuf> {
        let current = self.path().to_path_buf();
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }

        self.navigate_to(parent.clone());

        // Put the cursor on the directory we came out of, the way every file
        // manager does -- without it, `h` then `l` does not return you where
        // you were.
        if let Some(name) = current.file_name().and_then(|n| n.to_str()) {
            if let Some(index) = self.view.iter().position(|e| e.name == name) {
                self.cursor = index;
            }
        }

        Some(parent)
    }

    // ── Cursor ───────────────────────────────────────────────────────────

    pub fn move_cursor(&mut self, delta: isize) {
        if self.view.is_empty() {
            self.cursor = 0;
            return;
        }
        let last = self.view.len() - 1;
        // Saturating rather than wrapping: holding `j` should stop at the end
        // rather than jump back to the top.
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    pub fn cursor_to_top(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_to_bottom(&mut self) {
        self.cursor = self.view.len().saturating_sub(1);
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.view.len().saturating_sub(1));
    }

    // ── Marks ────────────────────────────────────────────────────────────

    pub fn is_marked(&self, name: &str) -> bool {
        self.marks.contains(name)
    }

    pub fn marks(&self) -> &HashSet<String> {
        &self.marks
    }

    /// Mark or unmark the selected entry, then step down -- so a run of
    /// entries can be marked by holding one key.
    pub fn toggle_mark(&mut self) {
        let Some(name) = self.selected().map(|e| e.name.clone()) else {
            return;
        };
        if !self.marks.remove(&name) {
            self.marks.insert(name);
        }
        self.move_cursor(1);
    }

    pub fn mark_all(&mut self) {
        self.marks = self.view.iter().map(|e| e.name.clone()).collect();
    }

    pub fn clear_marks(&mut self) {
        self.marks.clear();
    }

    pub fn invert_marks(&mut self) {
        self.marks = self
            .view
            .iter()
            .filter(|e| !self.marks.contains(&e.name))
            .map(|e| e.name.clone())
            .collect();
    }

    // ── View options ─────────────────────────────────────────────────────

    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.rebuild_view();
    }

    pub fn cycle_sort(&mut self) {
        self.sort.key = self.sort.key.next();
        self.rebuild_view();
    }

    pub fn reverse_sort(&mut self) {
        self.sort.reverse = !self.sort.reverse;
        self.rebuild_view();
    }

    pub fn sort_key(&self) -> SortKey {
        self.sort.key
    }

    // ── Search ───────────────────────────────────────────────────────────

    /// Move to the next entry whose name contains `query`, case-insensitively.
    ///
    /// Wraps, and returns false when nothing matches so the caller can say so
    /// rather than leaving the cursor sitting still with no explanation.
    pub fn search_from(&mut self, query: &str, forward: bool, skip_current: bool) -> bool {
        if self.view.is_empty() || query.is_empty() {
            return false;
        }

        let needle = query.to_lowercase();
        let count = self.view.len();
        let start = usize::from(skip_current);

        for step in start..=count {
            let index = if forward {
                (self.cursor + step) % count
            } else {
                (self.cursor + count - (step % count)) % count
            };
            if self.view[index].name.to_lowercase().contains(&needle) {
                self.cursor = index;
                return true;
            }
        }
        false
    }

    /// The error from the last failed listing, if any.
    pub fn error(&self) -> Option<&str> {
        match &self.state {
            PaneState::Failed(message) => Some(message),
            _ => None,
        }
    }

    /// Whether a path exists as a directory on this side.
    pub fn has_dir(&mut self, path: &Path) -> bool {
        self.vfs.is_dir(path)
    }

    /// Create a directory, for `on_missing = "create"`.
    pub fn create_dir(&mut self, path: &Path) -> Result<(), VfsError> {
        self.vfs.create_dir(path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::{tempdir, TempDir};

    use super::*;
    use crate::vfs::LocalFs;

    /// A pane over a temporary tree:
    ///
    /// ```text
    /// root/
    ///   .hidden
    ///   alpha.txt
    ///   beta.txt
    ///   sub/
    ///     nested.txt
    /// ```
    fn fixture() -> (TempDir, Pane) {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".hidden"), b"").unwrap();
        fs::write(dir.path().join("alpha.txt"), b"a").unwrap();
        fs::write(dir.path().join("beta.txt"), b"bb").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/nested.txt"), b"n").unwrap();

        let pane = Pane::new(
            Box::new(LocalFs::new()),
            dir.path().to_path_buf(),
            &Config::default(),
        );
        (dir, pane)
    }

    #[test]
    fn lists_the_starting_directory_with_dotfiles_hidden() {
        let (_dir, pane) = fixture();
        let names: Vec<_> = pane.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "alpha.txt", "beta.txt"]);
    }

    #[test]
    fn toggling_hidden_keeps_the_cursor_on_the_same_entry() {
        let (_dir, mut pane) = fixture();
        pane.move_cursor(1); // alpha.txt
        assert_eq!(pane.selected().unwrap().name, "alpha.txt");

        pane.toggle_hidden();

        assert_eq!(
            pane.selected().unwrap().name,
            "alpha.txt",
            "showing dotfiles must not slide the cursor onto another file"
        );
        assert_eq!(pane.len(), 4);
    }

    #[test]
    fn changing_the_sort_key_keeps_the_cursor_on_the_same_entry() {
        let (_dir, mut pane) = fixture();
        pane.move_cursor(2); // beta.txt
        assert_eq!(pane.selected().unwrap().name, "beta.txt");

        pane.cycle_sort();

        assert_eq!(pane.selected().unwrap().name, "beta.txt");
    }

    #[test]
    fn entering_and_leaving_a_directory_restores_the_cursor() {
        let (_dir, mut pane) = fixture();
        assert_eq!(pane.selected().unwrap().name, "sub");

        let entered = pane.enter_selected().unwrap();
        assert!(entered.ends_with("sub"));
        assert_eq!(pane.entries().len(), 1);

        pane.parent().unwrap();
        assert_eq!(
            pane.selected().unwrap().name,
            "sub",
            "going up should select the directory we came out of"
        );
    }

    #[test]
    fn entering_a_file_does_nothing() {
        let (_dir, mut pane) = fixture();
        pane.move_cursor(1); // alpha.txt
        assert!(pane.enter_selected().is_none());
        assert_eq!(pane.selected().unwrap().name, "alpha.txt");
    }

    #[test]
    fn the_cursor_stops_at_the_ends() {
        let (_dir, mut pane) = fixture();
        pane.move_cursor(-5);
        assert_eq!(pane.cursor(), 0);
        pane.move_cursor(99);
        assert_eq!(pane.cursor(), pane.len() - 1);
    }

    #[test]
    fn marking_steps_down_so_a_run_can_be_marked() {
        let (_dir, mut pane) = fixture();
        pane.toggle_mark();
        pane.toggle_mark();

        assert!(pane.is_marked("sub"));
        assert!(pane.is_marked("alpha.txt"));
        assert_eq!(pane.marks().len(), 2);
    }

    #[test]
    fn inverting_marks_selects_the_complement() {
        let (_dir, mut pane) = fixture();
        pane.toggle_mark(); // marks "sub"
        pane.invert_marks();

        assert!(!pane.is_marked("sub"));
        assert!(pane.is_marked("alpha.txt"));
        assert!(pane.is_marked("beta.txt"));
    }

    #[test]
    fn navigating_clears_marks_because_they_name_entries_in_one_directory() {
        let (_dir, mut pane) = fixture();
        pane.mark_all();
        assert!(!pane.marks().is_empty());

        pane.enter_selected();
        assert!(pane.marks().is_empty());
    }

    #[test]
    fn search_wraps_and_reports_failure() {
        let (_dir, mut pane) = fixture();
        assert!(pane.search_from("beta", true, false));
        assert_eq!(pane.selected().unwrap().name, "beta.txt");

        // Wrapping forward from the last entry finds the first match again.
        assert!(pane.search_from("sub", true, true));
        assert_eq!(pane.selected().unwrap().name, "sub");

        assert!(!pane.search_from("nothing-here", true, false));
    }

    #[test]
    fn search_is_case_insensitive() {
        let (_dir, mut pane) = fixture();
        assert!(pane.search_from("ALPHA", true, false));
        assert_eq!(pane.selected().unwrap().name, "alpha.txt");
    }

    /// A ghost pane shows nothing but still knows where it is, which is what
    /// lets synchronized browsing re-couple on the way back up.
    #[test]
    fn a_ghost_pane_keeps_its_virtual_path() {
        let (dir, mut pane) = fixture();
        let missing = dir.path().join("sub/does-not-exist");

        pane.ghost_at(missing.clone());

        assert!(pane.is_ghost());
        assert_eq!(pane.path(), missing);
        assert!(pane.is_empty());
        assert_eq!(pane.state, PaneState::Ghost);
    }

    #[test]
    fn relative_to_sync_root_tracks_descent() {
        let (_dir, mut pane) = fixture();
        assert_eq!(pane.relative_to_sync_root().unwrap(), Path::new(""));

        pane.enter_selected().unwrap();
        assert_eq!(pane.relative_to_sync_root().unwrap(), Path::new("sub"));
    }

    /// Going above the anchor has no relative path; synchronized browsing
    /// re-anchors rather than inventing one.
    #[test]
    fn relative_to_sync_root_is_none_above_the_anchor() {
        let (_dir, mut pane) = fixture();
        pane.parent().unwrap();
        assert!(pane.relative_to_sync_root().is_none());
    }

    #[test]
    fn an_unreadable_start_directory_falls_back_to_the_root() {
        let pane = Pane::new(
            Box::new(LocalFs::new()),
            PathBuf::from("/definitely/not/here"),
            &Config::default(),
        );
        assert_eq!(pane.real_path(), Path::new("/"));
    }
}
