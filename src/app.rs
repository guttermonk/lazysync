//! Application state and the action dispatch.
//!
//! Nothing here looks at raw key events. The event loop turns a key press
//! into an [`Action`] using the keymap for the current [`Mode`], and this
//! module decides what that action does. The one exception is text input,
//! which [`Mode::takes_text_input`] routes to the overlay as characters --
//! otherwise typing a password would run commands.

use std::path::PathBuf;

use crate::{
    browser::{
        sync_browse::{self, Followed},
        union::{self, UnionRow},
        Pane, Side,
    },
    config::{Config, Palette, PaneSide},
    keys::{Action, Keymaps, Mode},
    vfs::LocalFs,
};

/// What is on top of the browser, if anything.
///
/// The active [`Mode`] is derived from this rather than stored separately, so
/// the keymap in effect and what is on screen cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    /// The help screen, generated from the live keymap.
    Help {
        scroll: usize,
    },
    /// A single-line text prompt.
    Prompt(Prompt),
    /// A yes/no question, used by `sync_browse.on_missing = "ask"`.
    Question(Question),
}

/// A text prompt and what to do with what is typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Incremental search within the focused pane.
    Search,
}

impl PromptKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Search => "/",
        }
    }
}

/// A yes/no question, and what each answer does.
///
/// Both answers act. Declining still has to leave the panes coupled -- if "no"
/// merely dismissed the dialog, synchronized browsing would desync silently,
/// which is the failure this whole design is built to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub text: String,
    pub on_yes: Pending,
    pub on_no: Pending,
}

/// Work deferred until the user answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Create a directory in the pane at this index, then move there.
    CreateDir { pane: usize, path: PathBuf },
    /// Stand on the path as a ghost instead of creating it.
    GhostAt { pane: usize, path: PathBuf },
}

/// Everything the UI draws and the event loop mutates.
pub struct App {
    pub config: Config,
    pub keymaps: Keymaps,
    pub palette: Palette,

    pub panes: [Pane; 2],
    /// Index of the focused pane: 0 is local, 1 is remote.
    pub focus: usize,

    pub sync_browse: bool,
    pub union_view: bool,
    /// Shared cursor for union view, where both panes show the same rows.
    pub union_cursor: usize,
    /// Set when the panes could not be kept in step and the user should know.
    pub desynced: bool,

    pub overlay: Overlay,
    /// The status line message, cleared by the next action that sets one.
    pub status: String,
    pub should_quit: bool,
}

impl App {
    /// Build an app with both panes on the local filesystem.
    ///
    /// Phase 1 is local-to-local: the second pane gets a `LocalFs` too. Phase
    /// 2 replaces it with a connected backend without the browser changing.
    pub fn new(config: Config, left: PathBuf, right: PathBuf) -> anyhow::Result<Self> {
        let palette = config.theme.palette()?;
        let keymaps = config.keymaps();

        let panes = [
            Pane::new(Box::new(LocalFs::new()), left, &config),
            Pane::new(Box::new(LocalFs::new()), right, &config),
        ];

        let mut app = Self {
            focus: config.default_pane.index(),
            sync_browse: config.sync_browse.enabled,
            union_view: config.sync_browse.union_view,
            union_cursor: 0,
            desynced: false,
            overlay: Overlay::None,
            status: String::new(),
            should_quit: false,
            palette,
            keymaps,
            config,
            panes,
        };

        if app.sync_browse {
            sync_browse::anchor_both(&mut app.panes);
        }

        Ok(app)
    }

    /// Which keymap applies to the next key press.
    pub const fn mode(&self) -> Mode {
        match &self.overlay {
            Overlay::None => Mode::Browser,
            Overlay::Help { .. } => Mode::Help,
            Overlay::Prompt(_) => Mode::Prompt,
            // A yes/no question reuses the confirm keymap, so `y`/`n` mean the
            // same thing here as they will in the sync confirmation window.
            Overlay::Question(_) => Mode::Confirm,
        }
    }

    pub fn focused(&self) -> &Pane {
        &self.panes[self.focus]
    }

    pub fn focused_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.focus]
    }

    const fn other_index(&self) -> usize {
        1 - self.focus
    }

    /// Rows shown when union view is on.
    pub fn union_rows(&self) -> Vec<UnionRow> {
        union::merge(
            self.panes[0].entries(),
            self.panes[1].entries(),
            self.panes[self.focus].sort,
            self.panes[self.focus].dirs_first,
        )
    }

    /// How many rows the focused view has.
    fn row_count(&self) -> usize {
        if self.union_view {
            self.union_rows().len()
        } else {
            self.focused().len()
        }
    }

    // ── Dispatch ─────────────────────────────────────────────────────────

    /// Apply an action in the current mode.
    pub fn handle(&mut self, action: Action) {
        match self.mode() {
            Mode::Browser => self.handle_browser(action),
            Mode::Help => self.handle_help(action),
            Mode::Prompt => self.handle_prompt(action),
            Mode::Confirm => self.handle_question(action),
            // Phase 1 has no form, queue or log on screen, so these cannot be
            // reached; falling through to the browser would act on a pane the
            // user is not looking at.
            Mode::Form | Mode::Queue | Mode::Log => {}
        }
    }

    /// A printable character typed in a text-input mode.
    pub fn handle_text(&mut self, ch: char) {
        if let Overlay::Prompt(prompt) = &mut self.overlay {
            prompt.input.push(ch);
            // Search is incremental: every character moves the cursor, so you
            // see what you are typing towards.
            if prompt.kind == PromptKind::Search {
                let query = prompt.input.clone();
                self.search(&query, true, false);
            }
        }
    }

    /// Backspace in a text-input mode.
    pub fn handle_backspace(&mut self) {
        if let Overlay::Prompt(prompt) = &mut self.overlay {
            prompt.input.pop();
            if prompt.kind == PromptKind::Search && !prompt.input.is_empty() {
                let query = prompt.input.clone();
                self.search(&query, true, false);
            }
        }
    }

    fn handle_browser(&mut self, action: Action) {
        let half_page = 10_isize;
        let page = 20_isize;

        match action {
            // ── Motion ───────────────────────────────────────────────────
            Action::MoveDown => self.move_cursor(1),
            Action::MoveUp => self.move_cursor(-1),
            Action::HalfPageDown => self.move_cursor(half_page),
            Action::HalfPageUp => self.move_cursor(-half_page),
            Action::PageDown => self.move_cursor(page),
            Action::PageUp => self.move_cursor(-page),
            Action::MoveTop => self.cursor_to(0),
            Action::MoveBottom => self.cursor_to(self.row_count().saturating_sub(1)),

            // ── Navigation ───────────────────────────────────────────────
            Action::EnterDir => self.enter(),
            Action::ParentDir => self.go_parent(),
            Action::GoHome => {
                if let Some(home) = self.focused_mut().vfs.home() {
                    self.navigate(home);
                } else {
                    self.status = "no home directory for this backend".into();
                }
            }
            Action::GoRoot => {
                let root = self.focused().vfs.root();
                self.navigate(root);
            }
            Action::Refresh => {
                for pane in &mut self.panes {
                    pane.reload();
                }
                self.status = "reloaded".into();
            }

            // ── Panes ────────────────────────────────────────────────────
            Action::NextPane | Action::PrevPane => {
                self.focus = self.other_index();
                self.status.clear();
            }
            Action::SyncOtherPaneHere => self.align_other_pane_once(),

            // ── View ─────────────────────────────────────────────────────
            Action::ToggleHidden => {
                // Both panes, not just the focused one: a comparison between
                // two listings filtered differently is misleading.
                for pane in &mut self.panes {
                    pane.toggle_hidden();
                }
                let shown = self.panes[0].show_hidden;
                self.status = format!("dotfiles {}", if shown { "shown" } else { "hidden" });
            }
            Action::ToggleSyncBrowse => self.toggle_sync_browse(),
            Action::ToggleUnionView => self.toggle_union_view(),
            Action::ToggleDirSizes => {
                // Needs a recursive walk per directory, which over a network
                // backend is slow enough that it belongs with the transfer
                // queue rather than on the render path.
                self.status = "recursive directory sizes arrive in phase 4".into();
            }
            Action::CycleSort => {
                for pane in &mut self.panes {
                    pane.cycle_sort();
                }
                self.status = format!("sorted by {}", self.focused().sort_key());
            }
            Action::ReverseSort => {
                for pane in &mut self.panes {
                    pane.reverse_sort();
                }
                let reversed = self.panes[0].sort.reverse;
                self.status = format!(
                    "sort order {}",
                    if reversed { "descending" } else { "ascending" }
                );
            }

            // ── Marks ────────────────────────────────────────────────────
            Action::ToggleMark => {
                if self.union_view {
                    self.status = "marks follow a single pane; leave union view first".into();
                } else {
                    self.focused_mut().toggle_mark();
                }
            }
            Action::MarkAll => self.focused_mut().mark_all(),
            Action::ClearMarks => self.focused_mut().clear_marks(),
            Action::InvertMarks => self.focused_mut().invert_marks(),

            // ── Search ───────────────────────────────────────────────────
            Action::Search => {
                self.overlay = Overlay::Prompt(Prompt {
                    kind: PromptKind::Search,
                    input: String::new(),
                });
            }
            Action::SearchNext => self.repeat_search(true),
            Action::SearchPrev => self.repeat_search(false),

            // ── General ──────────────────────────────────────────────────
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            Action::Cancel => {
                self.focused_mut().clear_marks();
                self.status.clear();
            }
            Action::Quit => self.should_quit = true,

            // Bound only in modes phase 1 does not display.
            _ => {}
        }
    }

    fn handle_help(&mut self, action: Action) {
        let Overlay::Help { scroll } = &mut self.overlay else {
            return;
        };

        match action {
            Action::MoveDown => *scroll += 1,
            Action::MoveUp => *scroll = scroll.saturating_sub(1),
            Action::HalfPageDown | Action::PageDown => *scroll += 10,
            Action::HalfPageUp | Action::PageUp => *scroll = scroll.saturating_sub(10),
            Action::MoveTop => *scroll = 0,
            // The help text's length is a render-time fact, so "to the bottom"
            // is clamped by the renderer rather than guessed at here.
            Action::MoveBottom => *scroll = usize::MAX,
            Action::Cancel => self.overlay = Overlay::None,
            Action::Quit => self.should_quit = true,
            _ => {}
        }
    }

    fn handle_prompt(&mut self, action: Action) {
        match action {
            Action::Submit => {
                if let Overlay::Prompt(prompt) = &self.overlay {
                    let query = prompt.input.clone();
                    // Remember the query so `n`/`N` have something to repeat.
                    self.focused_mut().search = Some(query);
                }
                self.overlay = Overlay::None;
            }
            Action::Cancel => {
                self.overlay = Overlay::None;
                self.status.clear();
            }
            _ => {}
        }
    }

    fn handle_question(&mut self, action: Action) {
        let Overlay::Question(question) = &self.overlay else {
            return;
        };

        let pending = match action {
            Action::Apply | Action::Submit => question.on_yes.clone(),
            Action::Cancel => question.on_no.clone(),
            _ => return,
        };

        self.overlay = Overlay::None;
        self.run_pending(pending);
    }

    fn run_pending(&mut self, pending: Pending) {
        match pending {
            Pending::CreateDir { pane, path } => match self.panes[pane].create_dir(&path) {
                Ok(()) => {
                    self.panes[pane].navigate_to(path.clone());
                    self.status = format!("created {}", path.display());
                }
                Err(e) => {
                    // A failed mkdir ghosts rather than stranding the pane, so
                    // backing out still re-couples.
                    self.panes[pane].ghost_at(path);
                    self.status = format!("could not create it: {e}");
                }
            },
            Pending::GhostAt { pane, path } => {
                self.panes[pane].ghost_at(path.clone());
                self.status = format!("{} does not exist on the other side", path.display());
            }
        }
    }

    // ── Movement helpers ─────────────────────────────────────────────────

    fn move_cursor(&mut self, delta: isize) {
        if self.union_view {
            let count = self.row_count();
            if count == 0 {
                self.union_cursor = 0;
                return;
            }
            self.union_cursor = self
                .union_cursor
                .saturating_add_signed(delta)
                .min(count - 1);
        } else {
            self.focused_mut().move_cursor(delta);
        }
    }

    fn cursor_to(&mut self, index: usize) {
        if self.union_view {
            self.union_cursor = index.min(self.row_count().saturating_sub(1));
        } else if index == 0 {
            self.focused_mut().cursor_to_top();
        } else {
            self.focused_mut().cursor_to_bottom();
        }
    }

    // ── Navigation helpers ───────────────────────────────────────────────

    /// Enter the selected directory, carrying the other pane along when
    /// synchronized browsing is on.
    fn enter(&mut self) {
        if self.union_view {
            self.enter_union_row();
            return;
        }

        if self.focused_mut().enter_selected().is_none() {
            // A file, not a directory. Opening files arrives with the editor
            // integration; saying so beats doing nothing silently.
            if self.focused().selected().is_some() {
                self.status = "opening files arrives in phase 7".into();
            }
            return;
        }

        self.after_leader_moved();
    }

    /// Enter a row in union view.
    ///
    /// Both panes move: the side that has the directory navigates into it,
    /// and the side that does not ghosts at the same relative path. That is
    /// what makes a remote-only directory reachable while keeping the panes
    /// coupled.
    fn enter_union_row(&mut self) {
        let rows = self.union_rows();
        let Some(row) = rows.get(self.union_cursor) else {
            return;
        };
        if !row.is_dir_like() {
            self.status = "not a directory".into();
            return;
        }

        let name = row.name.clone();
        for index in 0..2 {
            let target = self.panes[index].path().join(&name);
            if self.panes[index].has_dir(&target) {
                self.panes[index].navigate_to(target);
            } else {
                self.panes[index].ghost_at(target);
            }
        }

        self.union_cursor = 0;
        self.desynced = false;
        self.status = format!("entered {name}");
    }

    fn go_parent(&mut self) {
        if self.union_view {
            for pane in &mut self.panes {
                pane.parent();
            }
            self.union_cursor = 0;
            return;
        }

        if self.focused_mut().parent().is_none() {
            self.status = "already at the root".into();
            return;
        }
        self.after_leader_moved();
    }

    fn navigate(&mut self, path: PathBuf) {
        self.focused_mut().navigate_to(path);
        self.after_leader_moved();
    }

    /// Keep the other pane in step after the focused pane moved.
    fn after_leader_moved(&mut self) {
        if !self.sync_browse {
            return;
        }

        let leader_index =
            sync_browse::leader_side(self.config.sync_browse.leader, Side::from_index(self.focus))
                .index();

        // Only the leader's movement propagates. With `leader = "local"`,
        // navigating the remote pane deliberately does not drag the local one.
        if leader_index != self.focus {
            return;
        }

        let follower_index = 1 - leader_index;
        let outcome = self.follow(leader_index);
        tracing::debug!("follower pane {follower_index}: {outcome:?}");
        self.report(outcome, follower_index);
    }

    /// Run the follow, splitting the pane array so leader and follower can
    /// both be borrowed mutably.
    fn follow(&mut self, leader: usize) -> Followed {
        let on_missing = self.config.sync_browse.on_missing;
        let (first, second) = self.panes.split_at_mut(1);

        if leader == 0 {
            sync_browse::follow(&mut first[0], &mut second[0], on_missing)
        } else {
            sync_browse::follow(&mut second[0], &mut first[0], on_missing)
        }
    }

    fn report(&mut self, outcome: Followed, follower: usize) {
        self.desynced = matches!(outcome, Followed::Desynced);

        match outcome {
            Followed::Moved(_) | Followed::Reanchored => self.status.clear(),
            // No path in these two: the status bar already shows the shared
            // relative path, and an absolute temp path would push everything
            // else off the line.
            Followed::Ghosted(_) => {
                self.status = format!("not present in the {} pane", side_label(follower));
            }
            Followed::Created(path) => {
                self.status = format!(
                    "created {}",
                    path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned()
                    )
                );
            }
            Followed::Desynced => {
                self.status = "panes are out of step".into();
            }
            Followed::NeedsPermission(path) => {
                self.overlay = Overlay::Question(Question {
                    text: format!("{} does not exist here. Create it?", path.display()),
                    on_yes: Pending::CreateDir {
                        pane: follower,
                        path: path.clone(),
                    },
                    on_no: Pending::GhostAt {
                        pane: follower,
                        path,
                    },
                });
            }
        }
    }

    /// Move the other pane here once, without turning synchronized browsing
    /// on. The quickest way to line two trees up before a sync.
    fn align_other_pane_once(&mut self) {
        let other = self.other_index();
        self.panes[self.focus].reanchor();
        self.panes[other].reanchor();

        let here = self.panes[self.focus].path().to_path_buf();
        self.status = format!("other pane anchored to {}", here.display());
    }

    fn toggle_sync_browse(&mut self) {
        self.sync_browse = !self.sync_browse;

        if self.sync_browse {
            // Anchor where the panes already are, rather than yanking one of
            // them somewhere the user did not ask to go.
            sync_browse::anchor_both(&mut self.panes);
            self.desynced = false;
            self.status = "synchronized browsing on".into();
        } else {
            self.status = "synchronized browsing off".into();
        }
    }

    fn toggle_union_view(&mut self) {
        self.union_view = !self.union_view;
        self.union_cursor = 0;

        if self.union_view {
            // Union rows are only meaningful against a shared anchor, so turn
            // synchronized browsing on with it rather than merging two
            // unrelated directories.
            if !self.sync_browse {
                self.sync_browse = true;
                sync_browse::anchor_both(&mut self.panes);
            }
            self.status = "union view on: L local only, R remote only".into();
        } else {
            self.status = "union view off".into();
        }
    }

    // ── Search helpers ───────────────────────────────────────────────────

    fn search(&mut self, query: &str, forward: bool, skip_current: bool) {
        let found = self.focused_mut().search_from(query, forward, skip_current);
        if !found {
            self.status = format!("no match for {query:?}");
        } else {
            self.status.clear();
        }
    }

    fn repeat_search(&mut self, forward: bool) {
        let Some(query) = self.focused().search.clone() else {
            self.status = "nothing to search for yet".into();
            return;
        };
        self.search(&query, forward, true);
    }
}

/// Which side a pane index refers to, for titles.
pub const fn side_label(index: usize) -> &'static str {
    if index == PaneSide::Local.index() {
        "local"
    } else {
        "remote"
    }
}
