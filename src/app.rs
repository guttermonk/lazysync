//! Application state and the action dispatch.
//!
//! Nothing here looks at raw key events. The event loop turns a key press
//! into an [`Action`] using the keymap for the current [`Mode`], and this
//! module decides what that action does. The one exception is text input,
//! which [`Mode::takes_text_input`] routes to the overlay as characters --
//! otherwise typing a password would run commands.

use std::path::{Path, PathBuf};

use crate::{
    bookmarks::{secrets, Bookmark, BookmarkStore},
    browser::{
        sync_browse::{self, Followed},
        union::{self, UnionRow},
        Pane, Side,
    },
    config::{Config, Executor, Palette, PaneSide, SyncMode, Watch},
    connection::{Protocol, Secret},
    integrations::zide,
    keys::{Action, Keymaps, Mode, ModifiedKey},
    sync::{
        audit, bisync, delegate, diff,
        executor::{Outcome, Report},
        filters::Filters,
        plan::{Action as PlanAction, Plan, Resolution, Section, Side as PlanSide},
        progress,
        safety::{self, DeletePolicy},
        state::{Record, Snapshot},
        transfer::{Settings as TransferSettings, State as TransferState, Transfer},
        watch,
    },
    ui::{confirm, form::ConnectionForm, hint, HitMap, Target},
    vfs::{ftp, hostkey::HostKey, rclone, ssh, LocalFs, Vfs},
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    /// Incremental search within the focused pane.
    Search,
    /// Filter the bookmark list.
    FilterBookmarks,
    /// A password for a connection.
    ///
    /// Carries the bookmark name so that the prompt can say which connection
    /// it is for -- the whole point of the open screen being just a name.
    Password { bookmark: String },
}

impl PromptKind {
    pub fn label(&self) -> String {
        match self {
            Self::Search | Self::FilterBookmarks => "/".into(),
            Self::Password { bookmark } => format!("password for {bookmark}: "),
        }
    }

    /// Whether what is typed should be hidden.
    pub const fn is_secret(&self) -> bool {
        matches!(self, Self::Password { .. })
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
    /// Which button is selected. Always starts on [`Answer::No`].
    pub answer: Answer,
}

impl Question {
    /// Ask, selecting "no".
    ///
    /// The selection is not a parameter. Every question lazysync asks guards
    /// something that cannot be undone from the dialog -- closing a
    /// connection, deleting a bookmark, applying a plan -- so the answer the
    /// window opens on is the one that does the least, and a caller cannot
    /// decide otherwise.
    pub fn new(text: impl Into<String>, on_yes: Pending, on_no: Pending) -> Self {
        Self {
            text: text.into(),
            on_yes,
            on_no,
            answer: Answer::No,
        }
    }
}

/// Which button of a [`Question`] is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
}

impl Answer {
    /// The other one. Two buttons, so "next" and "previous" are the same move.
    pub const fn other(self) -> Self {
        match self {
            Self::Yes => Self::No,
            Self::No => Self::Yes,
        }
    }
}

/// Work deferred until the user answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Create a directory in the pane at this index, then move there.
    CreateDir { pane: usize, path: PathBuf },
    /// Stand on the path as a ghost instead of creating it.
    GhostAt { pane: usize, path: PathBuf },
    /// Delete a saved connection, and forget any stored password with it.
    DeleteBookmark { name: String },
    /// Record the pending host key, then carry on opening the connection.
    TrustHostKey,
    /// Apply the plan under review, after a second confirmation.
    ApplyPlan,
    /// Close the open connection and return to the connection list.
    LeaveConnection,
    /// Nothing. The answer was "no" and no has no side effect.
    Nothing,
}

/// Which screen is in front.
///
/// Deliberately not a stack of arbitrary depth: there are three places to be,
/// and each knows where it returns to. A general stack would allow states like
/// a form on top of a form, which nothing here wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The saved connections. What lazysync opens on, and deliberately free of
    /// connection details -- those live only in the form.
    Bookmarks,
    /// The dual-pane browser.
    Browser,
    /// Adding or editing a connection.
    Form,
    /// Reviewing a sync plan before it is applied.
    Confirm,
    /// Applying a plan, with progress.
    Transfer,
    /// The audit log: what previous runs actually did.
    Log,
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

    /// How loudly to say it.
    ///
    /// Reset to [`StatusKind::Note`] at the top of every action, so a
    /// handler opts into prominence for the message it is about to set and
    /// cannot leave a stale colour behind for the next one. That is why this
    /// is a second field rather than part of the message: eighty-odd places
    /// assign a plain status, and all of them stay correct by doing nothing.
    pub status_kind: StatusKind,
    pub should_quit: bool,

    // ── Screens ──────────────────────────────────────────────────────────
    pub screen: Screen,

    /// Saved connections and recents.
    pub store: BookmarkStore,
    /// Cursor in the bookmark list.
    pub bookmark_cursor: usize,
    /// Substring filter on the bookmark list, from `/`.
    pub bookmark_filter: String,

    /// The connection form, while [`Screen::Form`] is in front.
    pub form: Option<ConnectionForm>,

    /// The sync plan under review, while [`Screen::Confirm`] is in front.
    pub plan: Option<Plan>,
    /// Cursor in the confirmation window's flattened heading-and-row list.
    pub plan_cursor: usize,
    /// Sections currently folded shut.
    pub folded: Vec<Section>,
    /// Which way the plan under review would move files.
    plan_mode: SyncMode,
    /// The pair a bisync plan belongs to: the local root and the remote's
    /// display form. Kept so the snapshot can be written against the same key
    /// it was read with, even if a pane has since moved.
    bisync_snapshot: Option<(PathBuf, String)>,
    /// The tree being watched, if watch mode is on.
    pub watcher: Option<watch::Watcher>,
    /// Which engine the run in flight is using, for the audit log.
    transfer_engine: &'static str,
    /// The audit log, read when the viewer opens rather than kept live: a
    /// log is appended to by other processes too, and a cached copy would go
    /// stale in the one place staleness is confusing.
    pub audit_entries: Vec<crate::sync::audit::Entry>,
    pub log_cursor: usize,
    /// Whether a human reviewed the plan in flight. False for watch mode's
    /// unattended syncs, which is the distinction the log exists to record.
    transfer_confirmed: bool,

    /// What the last applied run actually did.
    ///
    /// Needed by the bisync snapshot, which must record only the paths that
    /// genuinely reached agreement -- claiming one that failed would have the
    /// next run read the remaining difference as a fresh edit.
    last_report: Option<Report>,

    /// The transfer in flight, stepped by the main loop between frames.
    pub transfer: Option<Transfer>,

    /// Name of the sync profile the open connection references, if any.
    ///
    /// Set when a bookmark opens. It decides the exclude rules and the
    /// default direction, so a `[profiles.deploy]` entry is not just
    /// documentation for the headless path.
    pub active_profile: Option<String>,

    /// Where everything was on the last frame, so a click can be matched to
    /// what the user was pointing at. Written by `ui::draw`, read by the
    /// event loop, and rebuilt from nothing every frame.
    pub hits: HitMap,

    /// Where a tree walk reports to while it is running.
    ///
    /// [`progress::Silent`] unless something installs a real one: the event
    /// loop does, with a sink that animates a spinner and watches for the
    /// cancel key. Keeping it here rather than passing it down through
    /// `handle` means `App` never learns what a terminal is, and every
    /// headless path keeps the default that reports nothing.
    pub progress: Box<dyn progress::Progress>,

    /// The chord in progress: the keys pressed that have not yet resolved.
    ///
    /// Mirrored here from `event::Dispatcher` after every press, because
    /// widgets draw from `App` and never from the dispatcher. Without it the
    /// first key of a chord produced no evidence on screen at all -- `s` is
    /// bound to nothing on its own, so pressing it looked exactly like a key
    /// that had not arrived.
    pub pending_keys: Vec<ModifiedKey>,

    /// Which bookmark a password prompt or host key dialog belongs to.
    connecting: Option<Bookmark>,
    /// A host key awaiting approval, kept so that accepting it can record the
    /// key rather than having to re-read it from the server.
    host_key: Option<Box<HostKey>>,
}

impl App {
    /// Build an app with both panes on the local filesystem.
    ///
    /// Both panes start local even when the bookmark screen is in front: a
    /// connection replaces the right-hand pane when it opens, so the browser
    /// never has to cope with a pane that does not exist yet.
    pub fn new(config: Config, left: PathBuf, right: PathBuf) -> anyhow::Result<Self> {
        let palette = config.theme.palette()?;
        let keymaps = config.keymaps();

        let panes = [
            Pane::new(Box::new(LocalFs::new()), left, &config),
            Pane::new(Box::new(LocalFs::new()), right, &config),
        ];

        // A broken bookmarks file should not stop lazysync from starting: it
        // is still a usable local file manager, and refusing to launch would
        // leave no way to fix the file from here.
        let mut store = BookmarkStore::load_default().unwrap_or_else(|e| {
            tracing::warn!("ignoring unreadable bookmarks: {e}");
            BookmarkStore::default()
        });
        // After the load, and after the failure path above, so the connection
        // list is never empty even when the file is missing or unreadable.
        store.add_builtins();

        let mut app = Self {
            focus: config.default_pane.index(),
            sync_browse: config.sync_browse.enabled,
            union_view: config.sync_browse.union_view,
            union_cursor: 0,
            desynced: false,
            overlay: Overlay::None,
            status: String::new(),
            status_kind: StatusKind::Note,
            should_quit: false,
            screen: Screen::Browser,
            store,
            bookmark_cursor: 0,
            bookmark_filter: String::new(),
            form: None,
            plan: None,
            plan_cursor: 0,
            // The identical files start folded: a plan whose review list is
            // nine-tenths `=` rows is a plan nobody reads.
            folded: if config.confirm.fold_skipped {
                vec![Section::Skipped]
            } else {
                Vec::new()
            },
            plan_mode: SyncMode::Push,
            bisync_snapshot: None,
            watcher: None,
            transfer_engine: "native",
            audit_entries: Vec::new(),
            log_cursor: 0,
            transfer_confirmed: true,
            last_report: None,
            transfer: None,
            active_profile: None,
            hits: HitMap::default(),
            progress: Box::new(progress::Silent),
            pending_keys: Vec::new(),
            connecting: None,
            host_key: None,
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

    /// Open on the bookmark list instead of the browser.
    pub fn start_on_bookmarks(&mut self) {
        self.screen = Screen::Bookmarks;
    }

    /// Which keymap applies to the next key press.
    ///
    /// Derived from what is on screen rather than stored, so the keymap in
    /// effect and what the user is looking at cannot disagree.
    pub const fn mode(&self) -> Mode {
        // An overlay is in front of whatever screen is behind it, so it wins.
        match &self.overlay {
            Overlay::Help { .. } => return Mode::Help,
            Overlay::Prompt(_) => return Mode::Prompt,
            // The dialog has its own keymap rather than the confirmation
            // window's. Sharing one meant a layout that moved `n` onto a
            // motion -- which is what Colemak does -- silently took the
            // answer "no" with it, leaving a dialog whose own footer named a
            // key that did nothing.
            Overlay::Question(_) => return Mode::Question,
            Overlay::None => {}
        }

        match self.screen {
            Screen::Bookmarks => Mode::Bookmarks,
            Screen::Browser => Mode::Browser,
            Screen::Form => Mode::Form,
            Screen::Confirm => Mode::Confirm,
            Screen::Transfer => Mode::Queue,
            Screen::Log => Mode::Log,
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

    /// Say that a blocking action has started, if this is one of them.
    ///
    /// Returns whether anything was announced, which is the event loop's cue
    /// to draw a frame *before* dispatching.
    ///
    /// The need is structural: planning a sync walks both trees inside a
    /// single call -- one round trip per directory over SFTP -- and the loop
    /// cannot draw while that runs. `begin_sync` has always set "planning
    /// push...", and nobody has ever seen it: the next frame is drawn after
    /// the work it describes has finished, which is exactly when it stops
    /// being news. Meanwhile the terminal sits there not redrawing and not
    /// answering keys, which reads as a crash rather than as work.
    ///
    /// This is a stopgap and should be read as one. The real fix is a planner
    /// that yields the way [`Self::step_transfer`] does, at which point there
    /// is a spinner, a running count, and an Esc that works. Until then, this
    /// is the difference between a slow program and a frozen one.
    pub fn announce_work(&mut self, action: Action) -> bool {
        let Some(message) = self.working_on(action) else {
            return false;
        };

        self.status = message;
        self.status_kind = StatusKind::Answer;
        true
    }

    /// What to say before `action` blocks, or `None` if it does not.
    fn working_on(&self, action: Action) -> Option<String> {
        match (self.mode(), action) {
            (
                Mode::Browser,
                Action::SyncPush
                | Action::SyncPull
                | Action::SyncMirror
                | Action::SyncProfile
                | Action::SyncBisync,
            ) => Some(format!("planning {}...", action.label())),

            // Both open the selected connection -- see `handle_bookmarks`.
            // Named, because "opening..." on a list of connections leaves the
            // one question worth answering unanswered.
            (Mode::Bookmarks, Action::OpenItem | Action::EnterDir) => {
                Some(format!("opening {}...", self.selected_bookmark()?.name))
            }

            // Two listings, which on a remote with a large directory is its
            // own noticeable pause.
            (Mode::Browser, Action::Refresh) => Some("reloading...".into()),

            _ => None,
        }
    }

    /// Apply an action in the current mode.
    pub fn handle(&mut self, action: Action) {
        // Every action starts from "ambient". A handler that wants its
        // message to stand out says so while setting it; one that does not
        // cannot inherit the last action's emphasis.
        self.status_kind = StatusKind::Note;

        // Hiding is about the window rather than about what is in it, so it
        // is answered here instead of nine times over. Bound in one mode or
        // in all of them, it means the same thing and does the same thing --
        // which is also why it is safe above the dispatch: no screen has a
        // competing reading of "put this away".
        if action == Action::Hide {
            self.hide_window();
            return;
        }

        match self.mode() {
            Mode::Bookmarks => self.handle_bookmarks(action),
            Mode::Browser => self.handle_browser(action),
            Mode::Form => self.handle_form(action),
            Mode::Help => self.handle_help(action),
            Mode::Prompt => self.handle_prompt(action),
            Mode::Confirm => self.handle_confirm(action),
            Mode::Question => self.handle_question(action),
            Mode::Queue => self.handle_queue(action),
            Mode::Log => self.handle_log(action),
        }
    }

    /// Ask whatever is hosting the popup to stop showing it.
    ///
    /// Nothing here closes or forgets anything, and that is the feature: a
    /// hidden lazysync is a running one, so the connection, the marks, the
    /// cursor and a transfer in flight are all where you left them when the
    /// pane comes back. Quitting is the other key.
    ///
    /// A failure is reported as a `Problem` and left on the status line
    /// rather than announced, because of *when* it will be read. If the
    /// command worked, nobody is looking at this screen; if it did not, the
    /// line is still there whenever the user next glances at a window that
    /// did not go away, and by then the keystroke is the only thing it can
    /// be explaining.
    fn hide_window(&mut self) {
        match zide::hide(&self.config.zide.hide_command) {
            Ok(()) => self.status.clear(),
            Err(why) => {
                self.status = why;
                self.status_kind = StatusKind::Problem;
            }
        }
    }

    /// A printable character typed in a text-input mode.
    pub fn handle_text(&mut self, ch: char) {
        // A prompt is in front of the form, so it gets the character first.
        if let Overlay::Prompt(prompt) = &mut self.overlay {
            prompt.input.push(ch);

            // Search and filtering are incremental: every character moves the
            // cursor, so you can see what you are typing towards. A password
            // obviously is not.
            match prompt.kind.clone() {
                PromptKind::Search => {
                    let query = prompt.input.clone();
                    self.search(&query, true, false);
                }
                PromptKind::FilterBookmarks => {
                    self.bookmark_filter = prompt.input.clone();
                    self.clamp_bookmark_cursor();
                }
                PromptKind::Password { .. } => {}
            }
            return;
        }

        if let Some(form) = &mut self.form {
            form.insert(ch);
        }
    }

    /// Backspace in a text-input mode.
    pub fn handle_backspace(&mut self) {
        if let Overlay::Prompt(prompt) = &mut self.overlay {
            prompt.input.pop();

            match prompt.kind.clone() {
                PromptKind::Search => {
                    let query = prompt.input.clone();
                    if !query.is_empty() {
                        self.search(&query, true, false);
                    }
                }
                PromptKind::FilterBookmarks => {
                    self.bookmark_filter = prompt.input.clone();
                    self.clamp_bookmark_cursor();
                }
                PromptKind::Password { .. } => {}
            }
            return;
        }

        if let Some(form) = &mut self.form {
            form.backspace();
        }
    }

    // ── Mouse ────────────────────────────────────────────────────────────

    /// A click on something the last frame drew.
    ///
    /// One click selects; a second one on the same thing acts. The split is
    /// what every file manager does, and here it earns its keep twice over:
    /// the confirmation window is a screen where a stray click that acted
    /// could exclude a row from a sync without anybody noticing, and the
    /// browser is one where it could walk into a remote directory and spend
    /// a listing on it.
    ///
    /// The selection is applied here; the *action* a double click asks for is
    /// returned rather than run, so that it goes through the event loop's
    /// dispatch like a keypress does. That is not ceremony: opening a
    /// connection blocks for seconds, and the loop is where the frame saying
    /// so gets drawn.
    ///
    /// What is returned is deliberately an *existing* action rather than new
    /// behaviour, so the mouse can never do something the keyboard cannot.
    #[must_use = "a double click asks for an action; dropping it makes it a single click"]
    pub fn handle_click(&mut self, target: Target, double: bool) -> Option<Action> {
        self.status_kind = StatusKind::Note;

        match target {
            // The help overlay has nothing to select, and a shield exists
            // precisely so that the click stops here.
            Target::Blocked | Target::Help => None,

            Target::Pane { pane, row } => {
                if self.screen != Screen::Browser {
                    return None;
                }

                // Clicking into a pane focuses it, which is the half of this
                // that matters most: it is `tab` without aiming for the key.
                if self.focus != pane {
                    self.focus = pane;
                    self.status.clear();
                }

                let row = row?;

                if self.union_view {
                    self.union_cursor = row.min(self.row_count().saturating_sub(1));
                } else {
                    self.panes[pane].set_cursor(row);
                }

                double.then_some(Action::EnterDir)
            }

            Target::Bookmark(index) => {
                self.bookmark_cursor = index;
                self.clamp_bookmark_cursor();
                double.then_some(Action::OpenItem)
            }

            Target::Field(index) => {
                let form = self.form.as_mut()?;
                let field = form.fields.get(index)?;
                // A dimmed field is not reachable by keyboard either -- the
                // auth method it belongs to is not the one selected -- so a
                // click must not be the one way to land on it.
                if !form.is_relevant(field.id) {
                    return None;
                }
                form.cursor = index;

                // Cycling a choice is what "opening" a form row means. On a
                // text field this is a no-op, which is right: double-clicking
                // a name should not rewrite it.
                double.then_some(Action::NextChoice)
            }

            Target::PlanEntry(index) => {
                self.plan_cursor = index;
                // `ToggleRow` already means "fold" on a heading and "include
                // or exclude" on a row, so the mouse inherits both without a
                // second rule about which is which.
                double.then_some(Action::ToggleRow)
            }

            Target::LogEntry(index) => {
                self.log_cursor = index.min(self.audit_entries.len().saturating_sub(1));
                None
            }

            Target::Answer(answer) => {
                let Overlay::Question(question) = &mut self.overlay else {
                    return None;
                };
                question.answer = answer;
                // Two clicks to confirm, same as everywhere else. On this
                // dialog in particular -- it only ever guards something that
                // cannot be taken back -- that is a feature.
                double.then_some(Action::Submit)
            }
        }
    }

    /// The wheel, over whatever is under the pointer.
    ///
    /// It never changes focus: looking at the other pane should not take you
    /// out of the one you are working in, and a wheel is easy to nudge.
    pub fn handle_scroll(&mut self, target: Target, down: bool) {
        /// Lines per notch. Three is the usual compromise: one feels broken,
        /// a page loses your place.
        const LINES: isize = 3;

        let delta = if down { LINES } else { -LINES };

        match target {
            Target::Blocked | Target::Answer(_) => {}

            Target::Pane { pane, .. } => {
                if self.union_view {
                    // One shared cursor, so which pane it was over makes no
                    // difference.
                    self.move_cursor(delta);
                } else {
                    self.panes[pane].move_cursor(delta);
                }
            }

            Target::Bookmark(_) => self.move_bookmark_cursor(delta),

            Target::Field(_) => {
                if let Some(form) = &mut self.form {
                    form.move_cursor(delta);
                }
            }

            Target::PlanEntry(_) => {
                let Some(plan) = &self.plan else { return };
                let last = confirm::entries(plan, &self.folded).len().saturating_sub(1);
                self.plan_cursor = self.plan_cursor.saturating_add_signed(delta).min(last);
            }

            Target::LogEntry(_) => {
                let last = self.audit_entries.len().saturating_sub(1);
                self.log_cursor = self.log_cursor.saturating_add_signed(delta).min(last);
            }

            Target::Help => {
                if let Overlay::Help { scroll } = &mut self.overlay {
                    // No upper clamp here: only the renderer knows how long
                    // the help turned out to be, and it clamps there.
                    *scroll = scroll.saturating_add_signed(delta);
                }
            }
        }
    }

    // ── Bookmarks screen ─────────────────────────────────────────────────

    /// The bookmarks matching the current filter, with their store indices.
    pub fn visible_bookmarks(&self) -> Vec<(usize, &Bookmark)> {
        let needle = self.bookmark_filter.to_lowercase();
        self.store
            .bookmarks
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                needle.is_empty()
                    || b.name.to_lowercase().contains(&needle)
                    || b.host.to_lowercase().contains(&needle)
            })
            .collect()
    }

    pub fn selected_bookmark(&self) -> Option<&Bookmark> {
        self.visible_bookmarks()
            .get(self.bookmark_cursor)
            .map(|(_, b)| *b)
    }

    fn clamp_bookmark_cursor(&mut self) {
        let count = self.visible_bookmarks().len();
        self.bookmark_cursor = self.bookmark_cursor.min(count.saturating_sub(1));
    }

    fn handle_bookmarks(&mut self, action: Action) {
        match action {
            Action::MoveDown => self.move_bookmark_cursor(1),
            Action::MoveUp => self.move_bookmark_cursor(-1),
            Action::HalfPageDown | Action::PageDown => self.move_bookmark_cursor(10),
            Action::HalfPageUp | Action::PageUp => self.move_bookmark_cursor(-10),
            Action::MoveTop => self.bookmark_cursor = 0,
            Action::MoveBottom => {
                self.bookmark_cursor = self.visible_bookmarks().len().saturating_sub(1);
            }

            Action::OpenItem | Action::EnterDir => self.open_selected_bookmark(),

            Action::NewItem => {
                self.form = Some(ConnectionForm::new());
                self.screen = Screen::Form;
                self.status = "new connection".into();
            }
            // The built-in entry has nothing to edit and cannot be removed.
            // Both say so rather than opening a form over fields that mean
            // nothing, or asking a question whose yes would do nothing.
            Action::EditItem => match self.selected_bookmark() {
                Some(bookmark) if bookmark.builtin => {
                    self.status = format!("{} is built in -- nothing to configure", bookmark.name);
                    self.status_kind = StatusKind::Problem;
                }
                Some(bookmark) => {
                    self.form = Some(ConnectionForm::edit(bookmark));
                    self.screen = Screen::Form;
                    self.status.clear();
                }
                None => self.status = "nothing to edit".into(),
            },
            Action::DeleteItem => match self.selected_bookmark() {
                Some(bookmark) if bookmark.builtin => {
                    self.status = format!("{} is built in and cannot be deleted", bookmark.name);
                    self.status_kind = StatusKind::Problem;
                }
                Some(bookmark) => {
                    let name = bookmark.name.clone();
                    self.overlay = Overlay::Question(Question::new(
                        format!("Delete the connection {name:?}?"),
                        Pending::DeleteBookmark { name },
                        Pending::Nothing,
                    ));
                }
                None => self.status = "nothing to delete".into(),
            },

            Action::Search => {
                self.overlay = Overlay::Prompt(Prompt {
                    kind: PromptKind::FilterBookmarks,
                    input: self.bookmark_filter.clone(),
                });
            }

            Action::Refresh => match BookmarkStore::load_default() {
                Ok(store) => {
                    self.store = store;
                    self.clamp_bookmark_cursor();
                    self.status = "reloaded bookmarks".into();
                }
                Err(e) => self.status = format!("could not reload: {e}"),
            },

            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            Action::Cancel => {
                // Clears the filter, and otherwise does nothing: this is the
                // first screen, so there is nothing behind it.
                //
                // It used to drop to the two-pane browser, which is how you
                // reached a local-to-local sync. That is now the "Local
                // transfer" entry in this very list -- a row you can read
                // before you open it, rather than a screen you arrive at by
                // pressing Escape and have to deduce.
                if self.bookmark_filter.is_empty() {
                    self.status.clear();
                } else {
                    self.bookmark_filter.clear();
                    self.clamp_bookmark_cursor();
                }
            }
            Action::Quit => self.should_quit = true,

            _ => {}
        }
    }

    fn move_bookmark_cursor(&mut self, delta: isize) {
        let count = self.visible_bookmarks().len();
        if count == 0 {
            self.bookmark_cursor = 0;
            return;
        }
        self.bookmark_cursor = self
            .bookmark_cursor
            .saturating_add_signed(delta)
            .min(count - 1);
    }

    /// Start opening the selected bookmark.
    fn open_selected_bookmark(&mut self) {
        let Some(bookmark) = self.selected_bookmark().cloned() else {
            self.status = "no connections yet -- press n to add one".into();
            return;
        };
        self.begin_open(&bookmark);
    }

    /// Step one of opening a connection: establish who the server is.
    ///
    /// The host key is checked *before* any secret is collected. Doing it the
    /// other way round would mean typing a password at a server whose identity
    /// has not been established -- and would also mean holding that password
    /// across a dialog, which is exactly what should not happen.
    pub fn begin_open(&mut self, bookmark: &Bookmark) {
        let target = bookmark.target();

        // Local needs no handshake, no credential and no connection. It is
        // handled first so none of the machinery below has to carry a
        // "unless it is local" clause.
        if matches!(target.protocol, crate::connection::Protocol::Local) {
            self.open_local_transfer(bookmark);
            return;
        }

        if !target.protocol.is_implemented() {
            self.status = format!(
                "{} is not supported yet: {}",
                target.protocol,
                target
                    .protocol
                    .unavailable_because()
                    .unwrap_or("see docs/PLAN.md")
            );
            return;
        }

        // Only the SSH family has a host key to check. For the others there is
        // nothing to establish before collecting a credential -- which is
        // itself worth knowing: an FTP password goes to whatever answered on
        // port 21, and FTPS is the only version of that with a server identity
        // to verify at all.
        if !target.protocol.is_ssh() {
            self.collect_secret_and_open(bookmark);
            return;
        }

        self.status = format!("checking the host key for {}...", bookmark.name);

        match ssh::precheck(&target) {
            Ok(ssh::Precheck::Trusted) => self.collect_secret_and_open(bookmark),
            Ok(ssh::Precheck::Unknown {
                fingerprint,
                host_port,
                key,
            }) => {
                self.connecting = Some(bookmark.clone());
                self.host_key = Some(key);
                self.overlay = Overlay::Question(Question::new(
                    format!(
                        "{host_port} is not in known_hosts. Fingerprint {fingerprint}. \
                         Check it against the server before trusting it. Trust and continue?"
                    ),
                    Pending::TrustHostKey,
                    Pending::Nothing,
                ));
            }
            Ok(ssh::Precheck::Changed {
                fingerprint,
                host_port,
                known_hosts,
            }) => {
                // Deliberately *not* a yes/no question. A changed host key is
                // either a rebuilt server or someone in the middle, and
                // offering the same casual prompt as a first connection is how
                // people get trained to click through it. Clearing it means
                // editing known_hosts by hand, on purpose.
                self.connecting = None;
                self.host_key = None;
                self.status = format!(
                    "REFUSED: the host key for {host_port} changed (now {fingerprint}). If you \
                     expected this, remove its line from {}",
                    known_hosts.display()
                );
            }
            Err(e) => {
                self.connecting = None;
                self.host_key = None;
                self.status = format!("could not reach it: {e}");
            }
        }
    }

    /// Step two: get the secret, if one is needed at all.
    fn collect_secret_and_open(&mut self, bookmark: &Bookmark) {
        // A protocol with no credential of its own is opened straight away.
        // rclone keeps its own secrets, so prompting would ask for something
        // lazysync has nowhere to put.
        if !bookmark.protocol.needs_credentials() {
            self.finish_open(bookmark, None);
            return;
        }

        // The keyring first, then the prompt. Agent and key auth reach
        // neither, which is why opening a bookmark usually shows nothing but
        // its name.
        let stored = bookmark
            .remember_password
            .then(|| secrets::get(&bookmark.name))
            .flatten();

        if bookmark.needs_password_prompt() && stored.is_none() {
            self.connecting = Some(bookmark.clone());
            self.overlay = Overlay::Prompt(Prompt {
                kind: PromptKind::Password {
                    bookmark: bookmark.name.clone(),
                },
                input: String::new(),
            });
            return;
        }

        self.finish_open(bookmark, stored.as_ref());
    }

    /// Step three: open the session.
    fn finish_open(&mut self, bookmark: &Bookmark, secret: Option<&Secret>) {
        self.status = format!("connecting to {}...", bookmark.name);

        let target = bookmark.target();

        // Boxed as `dyn Vfs` because rclone's backend is a different type
        // from the `remotefs` adapter, and `open_connection` only ever needed
        // the trait.
        let opened: Result<Box<dyn Vfs>, _> = if target.protocol.is_ssh() {
            ssh::open(&target, secret).map(|fs| Box::new(fs) as Box<dyn Vfs>)
        } else if matches!(target.protocol, Protocol::Rclone) {
            // The host field carries the remote's name: `rclone://gdrive/x`
            // is the remote `gdrive`. There is no session to establish, so
            // this cannot fail for network reasons -- only for a remote that
            // is not in rclone.conf.
            rclone::RcloneFs::open(&target.host).map(|fs| Box::new(fs) as Box<dyn Vfs>)
        } else {
            ftp::open(&target, secret).map(|fs| Box::new(fs) as Box<dyn Vfs>)
        };

        match opened {
            Ok(fs) => self.open_connection(bookmark, fs, secret),
            Err(e) => {
                self.connecting = None;
                self.host_key = None;
                self.status = format!("could not connect: {e}");
            }
        }
    }

    /// Put a live connection in the right-hand pane and show the browser.
    fn open_connection(
        &mut self,
        bookmark: &Bookmark,
        mut fs: Box<dyn Vfs>,
        secret: Option<&Secret>,
    ) {
        // Where to start: the bookmark's remote path, then the remote home,
        // then the root. Opening `/` on a server where you only have a home
        // directory is technically correct and practically useless.
        let start = bookmark
            .remote_path
            .clone()
            .or_else(|| fs.home())
            .unwrap_or_else(|| PathBuf::from("/"));

        if let Some(local) = &bookmark.local_path {
            self.panes[0] = Pane::new(Box::new(LocalFs::new()), local.clone(), &self.config);
        }
        self.panes[1] = Pane::new(fs, start, &self.config);

        // Save the password only once the connection has actually worked --
        // storing a credential that turns out to be wrong is worse than not
        // storing one.
        if bookmark.remember_password {
            if let Some(secret) = secret.filter(|s| !s.is_empty()) {
                if let Err(e) = secrets::set(&bookmark.name, secret) {
                    self.status = e.to_string();
                }
            }
        }

        self.active_profile = bookmark.sync_profile.clone();
        self.sync_browse = bookmark.sync_browse || self.config.sync_browse.enabled;
        if self.sync_browse {
            sync_browse::anchor_both(&mut self.panes);
        }
        self.desynced = false;
        self.focus = self.config.default_pane.index();

        self.connecting = None;
        self.host_key = None;
        self.screen = Screen::Browser;
        if self.status.starts_with("connecting") {
            self.status = format!("connected to {}", bookmark.name);
        }
    }

    /// Where the open connection points, or `None` when both panes are local.
    ///
    /// Taken from the pane rather than from a remembered bookmark name,
    /// because the pane is what is actually open: a bookmark opened and then
    /// navigated elsewhere, or a connection opened from a URL, both have a
    /// live endpoint and no name worth quoting back.
    fn open_connection_label(&self) -> Option<String> {
        self.panes
            .iter()
            .find(|pane| pane.vfs.scheme() != "local")
            .map(|pane| pane.vfs.display_path(pane.path()))
    }

    /// Drop any open connection and go back to the connection list.
    ///
    /// The remote pane is genuinely replaced rather than just hidden behind
    /// another screen. "Close the connection" has to be true when it is what
    /// the user was asked -- and leaving a live session attached to a screen
    /// nothing can navigate back to is a socket held open for no reason.
    fn close_connection(&mut self) {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        for index in 0..self.panes.len() {
            if self.panes[index].vfs.scheme() != "local" {
                self.panes[index] = Pane::new(Box::new(LocalFs::new()), home.clone(), &self.config);
            }
        }

        // Synchronized browsing paired two specific roots; one of them no
        // longer exists. Leaving it on would couple a surviving pane to a
        // freshly-made one at home, which is a pairing nobody asked for.
        self.sync_browse = false;
        self.desynced = false;
        self.active_profile = None;

        self.screen = Screen::Bookmarks;
        self.status.clear();
    }

    /// Open the built-in local entry: two panes, both on this machine.
    ///
    /// Shares nothing with `finish_open` deliberately. That function's job is
    /// the aftermath of a *connection* -- storing a password that worked,
    /// clearing the host key, turning a "connecting..." status into a
    /// "connected" one -- and none of it applies. Threading a `None` through
    /// all of it would leave five conditionals to say "except when there is
    /// no server", which is how the local case ends up quietly wrong.
    fn open_local_transfer(&mut self, bookmark: &Bookmark) {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let left = bookmark.local_path.clone().unwrap_or_else(|| home.clone());
        let right = bookmark.remote_path.clone().unwrap_or(home);

        self.panes[0] = Pane::new(Box::new(LocalFs::new()), left, &self.config);
        self.panes[1] = Pane::new(Box::new(LocalFs::new()), right, &self.config);

        self.active_profile = bookmark.sync_profile.clone();
        // Not `|| config.sync_browse.enabled`, unlike a real connection. Two
        // panes that both start at home are already in step, so coupling them
        // means every move happens twice and the second pane becomes a mirror
        // you cannot steer -- the opposite of what you opened a two-pane
        // browser for. Turn it on with the key once the panes are where you
        // want them.
        self.sync_browse = bookmark.sync_browse;
        if self.sync_browse {
            sync_browse::anchor_both(&mut self.panes);
        }
        self.desynced = false;
        self.focus = self.config.default_pane.index();

        self.connecting = None;
        self.host_key = None;
        self.screen = Screen::Browser;
        self.status = "both panes are local -- pick a directory on each side".into();
    }

    // ── Connection form ──────────────────────────────────────────────────

    fn handle_form(&mut self, action: Action) {
        let Some(form) = &mut self.form else {
            self.screen = Screen::Bookmarks;
            return;
        };

        match action {
            Action::MoveDown => form.move_cursor(1),
            Action::MoveUp => form.move_cursor(-1),
            Action::NextChoice => form.cycle(true),
            Action::PrevChoice => form.cycle(false),

            Action::Submit | Action::Apply => self.save_form(),

            Action::Cancel => {
                self.form = None;
                self.screen = Screen::Bookmarks;
                self.status = "discarded".into();
            }
            Action::Quit => self.should_quit = true,
            _ => {}
        }
    }

    fn save_form(&mut self) {
        let Some(form) = &mut self.form else { return };

        let bookmark = match form.to_bookmark() {
            Ok(bookmark) => bookmark,
            Err(message) => {
                form.error = Some(message.clone());
                self.status = message;
                return;
            }
        };

        // A rename replaces the old entry rather than leaving a duplicate
        // behind -- and takes its stored password with it, so the secret does
        // not outlive the name it was filed under.
        let renamed_from = form.editing.clone().filter(|old| *old != bookmark.name);

        if let Some(old) = &renamed_from {
            self.store.remove(old);
            if let Err(e) = secrets::forget(old) {
                tracing::debug!("could not forget the password for {old}: {e}");
            }
        }

        // Switching away from password auth should not leave a secret behind.
        if !bookmark.remember_password {
            if let Err(e) = secrets::forget(&bookmark.name) {
                tracing::debug!("could not forget the password for {}: {e}", bookmark.name);
            }
        }

        let name = bookmark.name.clone();
        self.store.upsert(bookmark);

        match self.store.save_default() {
            Ok(()) => {
                self.status = format!("saved {name}");
                self.form = None;
                self.screen = Screen::Bookmarks;
                // Put the cursor on what was just saved, so editing again is
                // one keystroke away.
                if let Some(index) = self
                    .visible_bookmarks()
                    .iter()
                    .position(|(_, b)| b.name == name)
                {
                    self.bookmark_cursor = index;
                }
            }
            Err(e) => {
                let message = format!("could not save: {e}");
                if let Some(form) = &mut self.form {
                    form.error = Some(message.clone());
                }
                self.status = message;
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
            Action::SyncPush => self.begin_sync(SyncMode::Push),
            Action::SyncPull => self.begin_sync(SyncMode::Pull),
            Action::SyncMirror => self.begin_sync(SyncMode::MirrorPush),
            Action::SyncProfile => self.begin_profile_sync(),
            Action::SyncBisync => self.begin_sync(SyncMode::Bisync),
            Action::ToggleWatch => self.toggle_watch(),
            Action::ShowLog => self.open_log(),

            // Leaving the browser with a connection open ends that session:
            // the connection list cannot navigate back to a live browser, so
            // the only route back is to open a bookmark again, which
            // reconnects. Asking first is the difference between a keystroke
            // that changes screens and one that drops a session -- and `c`
            // sits next to nothing destructive, so it is easy to hit.
            Action::ShowConnections => match self.open_connection_label() {
                Some(where_to) => {
                    self.overlay = Overlay::Question(Question::new(
                        format!("Close the connection to {where_to}?"),
                        Pending::LeaveConnection,
                        Pending::Nothing,
                    ));
                }
                // Two local panes: nothing to close, so nothing to ask.
                None => self.close_connection(),
            },

            Action::ToggleDirSizes => {
                // Needs a recursive walk per directory, which over a network
                // backend is slow enough that it belongs with the transfer
                // queue rather than on the render path.
                self.status = "recursive directory sizes are not implemented yet".into();
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
        let Overlay::Prompt(prompt) = &self.overlay else {
            return;
        };
        let kind = prompt.kind.clone();
        let input = prompt.input.clone();

        match action {
            Action::Submit => {
                self.overlay = Overlay::None;

                match kind {
                    PromptKind::Search => {
                        // Remember the query so `n`/`N` have something to
                        // repeat.
                        self.focused_mut().search = Some(input);
                    }
                    PromptKind::FilterBookmarks => {
                        self.bookmark_filter = input;
                        self.clamp_bookmark_cursor();
                    }
                    PromptKind::Password { .. } => {
                        let Some(bookmark) = self.connecting.clone() else {
                            self.status = "nothing was waiting for a password".into();
                            return;
                        };
                        // The secret lives exactly as long as this call: it is
                        // moved into `finish_open`, used, and dropped (zeroing
                        // itself) before the next frame is drawn.
                        let secret = Secret::new(input);
                        self.finish_open(&bookmark, Some(&secret));
                    }
                }
            }
            Action::Cancel => {
                self.overlay = Overlay::None;
                if matches!(kind, PromptKind::Password { .. }) {
                    self.connecting = None;
                    self.host_key = None;
                    self.status = "cancelled".into();
                } else {
                    self.status.clear();
                }
            }
            _ => {}
        }
    }

    // ── Sync: plan, review, apply ────────────────────────────────────────

    /// Build a plan and put it on screen.
    ///
    /// Nothing is transferred here. The plan *is* the dry run, and the
    /// confirmation window is the only route to applying it -- which is why
    /// there is no "sync now" that skips this.
    pub fn begin_sync(&mut self, mode: SyncMode) {
        if !mode.is_implemented() {
            self.status = format!(
                "{} arrives in a later phase -- see docs/PLAN.md",
                mode.label()
            );
            return;
        }

        if matches!(mode, SyncMode::Bisync) {
            self.begin_bisync();
            return;
        }

        let direction = mode.direction();
        let source_index = direction.source;
        let dest_index = direction.dest();

        // Roots are the panes' current directories, so what gets synced is
        // what you are looking at. Using the bookmark's configured paths
        // instead would sync something other than what is on screen.
        let source_root = self.panes[source_index].path().to_path_buf();
        let dest_root = self.panes[dest_index].path().to_path_buf();

        if self.panes[source_index].is_ghost() || self.panes[dest_index].is_ghost() {
            self.status =
                "one pane is standing on a directory that does not exist; nothing to sync".into();
            self.status_kind = StatusKind::Problem;
            return;
        }

        let source_label = side_label(source_index).to_string();
        let dest_label = side_label(dest_index).to_string();

        // Exclude rules from the active profile. A bad pattern stops the plan
        // rather than being skipped: a filter that silently does nothing would
        // let `target/` be pushed to a web server.
        let filters = match self.resolve_filters(&source_root) {
            Ok(filters) => filters,
            Err(message) => {
                self.status = message;
                return;
            }
        };

        self.status = format!("planning {}...", mode.label());

        // Marks on the *source* pane narrow the plan to what was marked.
        //
        // The source side, not the focused one: "push these" has to mean the
        // files you marked in the pane they are being sent from, or marking
        // in the destination would silently scope a transfer by names that
        // have nothing to do with it.
        //
        // Cloned because the panes are borrowed mutably just below, and a
        // handful of file names is not worth restructuring the borrow for.
        let scope: Option<diff::Scope> = {
            let marks = self.panes[source_index].marks();
            (!marks.is_empty()).then(|| marks.clone())
        };
        let scoped_names: Vec<String> = {
            let mut names: Vec<String> = scope.iter().flatten().cloned().collect();
            names.sort();
            names
        };

        // Split the array so both panes can be borrowed mutably at once.
        let (first, second) = self.panes.split_at_mut(1);
        let (source, dest) = if source_index == 0 {
            (&mut first[0], &mut second[0])
        } else {
            (&mut second[0], &mut first[0])
        };

        // Taken out for the duration rather than borrowed from `self`: the
        // panes are already borrowed mutably just above, and the sink has to
        // outlive that borrow to report from inside the walk.
        let mut progress = std::mem::replace(
            &mut self.progress,
            Box::new(progress::Silent) as Box<dyn progress::Progress>,
        );
        progress.start(&format!("planning {}", mode.label()));

        let mut plan = diff::plan_with_filters(
            diff::Endpoint {
                fs: source.vfs.as_mut(),
                root: &source_root,
                label: source_label,
            },
            diff::Endpoint {
                fs: dest.vfs.as_mut(),
                root: &dest_root,
                label: dest_label,
            },
            direction,
            &self.config,
            &filters,
            scope.as_ref(),
            progress.as_mut(),
        );

        progress.finish();
        self.progress = progress;

        if plan.cancelled {
            // Nothing was read that can be trusted as a whole, so nothing is
            // shown. Said out loud, because a sync key that silently returned
            // you to the browser would read as the key having failed.
            self.status = format!("{} cancelled -- nothing was scanned", mode.label());
            self.status_kind = StatusKind::Answer;
            return;
        }

        safety::check(&mut plan, &self.config.safety);

        if plan.is_empty() && plan.unreadable.is_empty() {
            self.status = nothing_to_do(mode, &scoped_names, plan.method, &self.config);
            self.status_kind = StatusKind::Answer;
            return;
        }

        self.plan_mode = mode;
        self.plan_cursor = 0;
        self.folded = if self.config.confirm.fold_skipped {
            vec![Section::Skipped]
        } else {
            Vec::new()
        };
        self.plan = Some(plan);
        self.screen = Screen::Confirm;
        self.status.clear();
    }

    /// Plan a two-way sync.
    ///
    /// Separate from [`App::begin_sync`] because it is symmetric: there is no
    /// source pane to arrange the arguments around, and the extra input is the
    /// stored snapshot rather than a direction.
    fn begin_bisync(&mut self) {
        let local_root = self.panes[0].path().to_path_buf();
        let remote_root = self.panes[1].path().to_path_buf();

        if self.panes[0].is_ghost() || self.panes[1].is_ghost() {
            self.status =
                "one pane is standing on a directory that does not exist; nothing to sync".into();
            self.status_kind = StatusKind::Problem;
            return;
        }

        let filters = match self.resolve_filters(&local_root) {
            Ok(filters) => filters,
            Err(message) => {
                self.status = message;
                return;
            }
        };

        // The snapshot is keyed by the pair of roots, including the remote's
        // display form -- so the same local directory bisynced against two
        // different servers keeps two histories rather than confusing them.
        let remote_key = self.panes[1].vfs.display_path(&remote_root);
        let snapshot = Snapshot::load(&local_root, &remote_key);
        let first_run = snapshot.is_first_run();

        self.status = "planning bisync...".into();

        let mut progress = std::mem::replace(
            &mut self.progress,
            Box::new(progress::Silent) as Box<dyn progress::Progress>,
        );
        progress.start("planning bisync");

        let (first, second) = self.panes.split_at_mut(1);
        let mut plan = bisync::plan(
            diff::Endpoint {
                fs: first[0].vfs.as_mut(),
                root: &local_root,
                label: side_label(0).to_string(),
            },
            diff::Endpoint {
                fs: second[0].vfs.as_mut(),
                root: &remote_root,
                label: side_label(1).to_string(),
            },
            &snapshot,
            &self.config,
            &filters,
            progress.as_mut(),
        );

        progress.finish();
        self.progress = progress;

        if plan.cancelled {
            // No snapshot is written either. A snapshot claims the two trees
            // were examined and agreed; a cancelled scan examined part of one.
            self.status = "bisync cancelled -- nothing was scanned".into();
            self.status_kind = StatusKind::Answer;
            return;
        }

        safety::check(&mut plan, &self.config.safety);

        if plan.is_empty() && plan.unreadable.is_empty() {
            // Still record the snapshot: the trees agree, and saying so is
            // what lets the *next* run tell a deletion from a creation. A
            // first bisync of two already-identical trees would otherwise
            // stay a first run for ever, never able to propagate a deletion.
            self.store_snapshot(&local_root, &remote_key, &plan);
            self.status = "nothing to bisync -- the trees already match".into();
            self.status_kind = StatusKind::Answer;
            return;
        }

        self.plan_mode = SyncMode::Bisync;
        self.bisync_snapshot = Some((local_root, remote_key));
        self.plan_cursor = 0;
        self.folded = if self.config.confirm.fold_skipped {
            vec![Section::Skipped]
        } else {
            Vec::new()
        };
        self.plan = Some(plan);
        self.screen = Screen::Confirm;

        self.status = if first_run {
            // Said out loud, because the run behaves differently and the
            // difference is a safety property rather than a detail.
            "first bisync of this pair: nothing will be deleted, since a file on one side only \
             is as likely to be new as removed"
                .into()
        } else {
            String::new()
        };
    }

    /// Settle the conflict under the cursor.
    fn resolve_conflict(&mut self, resolution: Resolution) {
        let Some(plan) = &mut self.plan else {
            return;
        };

        let entries = confirm::entries(plan, &self.folded);
        let Some(index) = confirm::row_at(&entries, self.plan_cursor) else {
            return;
        };

        let Some(row) = plan.rows.get_mut(index) else {
            return;
        };
        if !matches!(row.action, PlanAction::Conflict) {
            self.status = "that row is not a conflict".into();
            return;
        }

        row.resolution = Some(resolution);
        let path = row.path.clone();

        // Re-check safety: resolving a conflict can turn it into a write, and
        // a plan that was under the delete limit before a decision may not be
        // after it.
        safety::check(plan, &self.config.safety);

        let left = plan.undecided().count();
        self.status = if left > 0 {
            format!(
                "{}: {} -- {left} conflict{} still undecided",
                path.display(),
                resolution.label(),
                if left == 1 { "" } else { "s" }
            )
        } else {
            format!("{}: {}", path.display(), resolution.label())
        };
    }

    /// Record the agreed state of a pair after a bisync.
    ///
    /// Three kinds of row contribute. Rows already in agreement are recorded
    /// from the plan. Rows a run actually transferred are **re-stated on the
    /// side that was written**, because what landed there is not always what
    /// was sent: a backend that cannot set a modification time gives the file
    /// its own, and recording the source's instead would make the next run see
    /// an edit that never happened -- on a two-way sync, a phantom conflict.
    /// Rows that were deleted leave the history entirely.
    ///
    /// A row the run *failed* on is recorded nowhere. Claiming agreement for a
    /// file that did not transfer is the one entry that could cause real loss:
    /// the next run would read the still-present difference as a fresh edit and
    /// could propagate it the wrong way.
    fn store_snapshot(&mut self, local_root: &Path, remote_key: &str, plan: &Plan) {
        let mut snapshot = Snapshot::load(local_root, remote_key);

        // What the last run managed, by path. Absent when no transfer ran --
        // the trees already agreed -- in which case every `Skip` row still
        // deserves recording.
        let outcomes: std::collections::HashMap<&Path, &Outcome> = self
            .last_report
            .as_ref()
            .map(|report| {
                report
                    .applied
                    .iter()
                    .map(|a| (a.path.as_path(), &a.outcome))
                    .collect()
            })
            .unwrap_or_default();

        let local_root_owned = local_root.to_path_buf();
        let remote_root = self.panes[1].path().to_path_buf();

        for row in &plan.rows {
            let outcome = outcomes.get(row.path.as_path()).copied();

            match row.action {
                PlanAction::Skip => {
                    if let (Some(local), Some(remote)) = (row.side_of(0), row.side_of(1)) {
                        snapshot.insert(
                            &row.path,
                            Record {
                                local_size: local.size,
                                local_mtime: local.mtime,
                                remote_size: remote.size,
                                remote_mtime: remote.mtime,
                            },
                        );
                    }
                }

                PlanAction::Create | PlanAction::Overwrite | PlanAction::Conflict => {
                    // Only if it actually transferred.
                    if !matches!(outcome, Some(Outcome::Transferred { .. })) {
                        continue;
                    }

                    let target = crate::sync::transfer::written_pane(row);
                    let (root, other_root) = if target == 0 {
                        (&local_root_owned, &remote_root)
                    } else {
                        (&remote_root, &local_root_owned)
                    };
                    let _ = other_root;

                    let (first, second) = self.panes.split_at_mut(1);
                    let pane = if target == 0 {
                        &mut first[0]
                    } else {
                        &mut second[0]
                    };

                    let Ok(landed) = pane.vfs.stat(&root.join(&row.path)) else {
                        // Could not confirm what is there, so nothing is
                        // claimed about it. The next run will compare the
                        // files themselves, which is the honest fallback.
                        continue;
                    };

                    let sent = row.side_of(1 - target);
                    let (Some(sent), landed) = (sent, PlanSide::from_entry(&landed)) else {
                        continue;
                    };

                    let record = if target == 0 {
                        Record {
                            local_size: landed.size,
                            local_mtime: landed.mtime,
                            remote_size: sent.size,
                            remote_mtime: sent.mtime,
                        }
                    } else {
                        Record {
                            local_size: sent.size,
                            local_mtime: sent.mtime,
                            remote_size: landed.size,
                            remote_mtime: landed.mtime,
                        }
                    };
                    snapshot.insert(&row.path, record);
                }

                // Gone from the pair, so gone from its history. Recorded only
                // when the deletion actually happened.
                PlanAction::Delete => {
                    if outcome.is_none()
                        || matches!(
                            outcome,
                            Some(Outcome::Deleted | Outcome::MovedToBackup { .. })
                        )
                    {
                        snapshot.remove(&row.path);
                    }
                }

                PlanAction::CreateDir => {}
            }
        }

        if let Err(e) = snapshot.save(chrono::Utc::now()) {
            // Not fatal, but it does mean the next run is another first run,
            // so it is worth saying rather than logging quietly.
            self.status = format!("the sync ran, but its state could not be saved: {e}");
        }
    }

    /// Compile the exclude rules for the active profile.
    fn resolve_filters(&self, source_root: &Path) -> Result<Filters, String> {
        let Some(profile) = self
            .active_profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
        else {
            return Ok(Filters::default());
        };

        let mut patterns = profile.exclude.clone();

        if profile.respect_gitignore {
            // The sync root's own `.gitignore` only. Nested ones would need
            // per-directory rule stacks, which is a bigger piece of work than
            // it looks and is not worth half-doing: this covers the case
            // people actually mean, and the status line says what was loaded.
            let path = source_root.join(".gitignore");
            match std::fs::read_to_string(&path) {
                Ok(text) => patterns.extend(text.lines().map(str::to_owned)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::debug!("could not read {}: {e}", path.display());
                }
            }
        }

        Filters::parse(&patterns).map_err(|e| e.to_string())
    }

    /// The browser keys bound to `actions`, as `sp or sl`.
    ///
    /// `None` when none of them is bound, so a message naming an alternative
    /// drops the clause rather than pointing at a key that does nothing.
    fn keys_for(&self, actions: &[Action]) -> Option<String> {
        let keys: Vec<String> = actions
            .iter()
            .filter_map(|action| hint::key_for(self.keymaps.get(Mode::Browser), *action))
            .collect();

        match keys.as_slice() {
            [] => None,
            [only] => Some(only.clone()),
            [rest @ .., last] => Some(format!("{} or {last}", rest.join(", "))),
        }
    }

    /// Plan using the active profile's own mode.
    ///
    /// The point of a profile is not having to remember which direction a
    /// given connection syncs in.
    pub fn begin_profile_sync(&mut self) {
        let Some(name) = self.active_profile.clone() else {
            // Named from the keymap rather than spelled out: this message
            // used to say "use sp, sl or sm", which is wrong for anybody who
            // rebound them -- the same failure `ui::hint` exists to prevent.
            let mut message =
                String::from("this connection has no sync profile; set one in its editor");
            if let Some(keys) = self.keys_for(&[Action::SyncPush, Action::SyncPull]) {
                message.push_str(&format!(", or sync one way with {keys}"));
            }
            self.status = message;
            return;
        };
        let Some(profile) = self.config.profiles.get(&name) else {
            self.status = format!("no [profiles.{name}] in config.toml");
            return;
        };

        let mode = profile.mode;
        self.status = format!("profile {name}: {}", mode.label());
        self.begin_sync(mode);
    }

    fn handle_confirm(&mut self, action: Action) {
        // A yes/no question on top of this window is `Mode::Question` and is
        // dispatched there, so nothing about it is handled here.
        let Some(plan) = &mut self.plan else {
            self.screen = Screen::Browser;
            return;
        };

        let entries = confirm::entries(plan, &self.folded);
        let last = entries.len().saturating_sub(1);

        match action {
            Action::MoveDown => self.plan_cursor = (self.plan_cursor + 1).min(last),
            Action::MoveUp => self.plan_cursor = self.plan_cursor.saturating_sub(1),
            Action::HalfPageDown | Action::PageDown => {
                self.plan_cursor = (self.plan_cursor + 10).min(last);
            }
            Action::HalfPageUp | Action::PageUp => {
                self.plan_cursor = self.plan_cursor.saturating_sub(10);
            }
            Action::MoveTop => self.plan_cursor = 0,
            Action::MoveBottom => self.plan_cursor = last,

            Action::ToggleRow => {
                if let Some(index) = confirm::row_at(&entries, self.plan_cursor) {
                    plan.toggle(index);
                    // Re-check: excluding deletions can bring a refused plan
                    // back under the limit, and a refusal that will not clear
                    // is a dead end.
                    safety::check(plan, &self.config.safety);
                } else if let Some(section) = confirm::section_at(&entries, self.plan_cursor) {
                    // On a heading, space folds -- the obvious thing to want.
                    self.toggle_fold(section);
                }
            }
            Action::AllRows => {
                plan.set_all(true);
                safety::check(plan, &self.config.safety);
            }
            Action::InvertRows => {
                plan.invert();
                safety::check(plan, &self.config.safety);
            }
            Action::OnlyRow => {
                if let Some(index) = confirm::row_at(&entries, self.plan_cursor) {
                    plan.only(index);
                    safety::check(plan, &self.config.safety);
                }
            }

            Action::FoldSection => {
                if let Some(section) = confirm::section_at(&entries, self.plan_cursor) {
                    if !self.folded.contains(&section) {
                        self.folded.push(section);
                    }
                }
            }
            Action::UnfoldSection => {
                if let Some(section) = confirm::section_at(&entries, self.plan_cursor) {
                    self.folded.retain(|s| *s != section);
                }
            }

            Action::ResolveKeepLocal => self.resolve_conflict(Resolution::KeepLocal),
            Action::ResolveKeepRemote => self.resolve_conflict(Resolution::KeepRemote),
            Action::ResolveKeepBoth => self.resolve_conflict(Resolution::KeepBoth),
            Action::ResolveSkip => self.resolve_conflict(Resolution::Skip),

            Action::Checksum => {
                self.status = "re-checking by checksum arrives in a later phase".into();
            }
            Action::DiffRow => {
                self.status = "inline diffs arrive in a later phase".into();
            }

            Action::Apply => self.confirm_apply(),

            Action::Cancel => {
                self.plan = None;
                self.screen = Screen::Browser;
                self.status = "cancelled -- nothing was transferred".into();
            }
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            Action::Quit => self.should_quit = true,
            _ => {}
        }
    }

    fn toggle_fold(&mut self, section: Section) {
        if self.folded.contains(&section) {
            self.folded.retain(|s| *s != section);
        } else {
            self.folded.push(section);
        }
    }

    /// `y` in the confirmation window.
    ///
    /// A plan that deletes anything gets a second question. Not ceremony: the
    /// first keystroke agrees to a screenful of rows, and the one irreversible
    /// part of that deserves to be named on its own.
    fn confirm_apply(&mut self) {
        let Some(plan) = &self.plan else { return };

        if let Some(refusal) = &plan.refusal {
            self.status = refusal.clone();
            return;
        }
        if !plan.can_apply() {
            self.status = "nothing included -- press a to include everything".into();
            return;
        }

        let totals = plan.totals();
        let policy = DeletePolicy::from_config(&self.config.safety);

        if totals.deletes > 0 && matches!(policy, DeletePolicy::Remove) {
            // Which side is named has to come from the rows, not from the
            // mode: a bisync deletes on whichever side the file was removed
            // from, and can do both in one plan. Naming one side there would
            // understate what is about to happen.
            let mut panes: Vec<usize> = plan.destructive_rows().map(|r| r.target_pane).collect();
            panes.sort_unstable();
            panes.dedup();

            let where_ = match panes.as_slice() {
                [pane] => format!("the {} side", side_label(*pane)),
                _ => "both sides".to_string(),
            };

            self.overlay = Overlay::Question(Question::new(
                format!(
                    "Permanently delete {} file(s) on {where_}? This cannot be undone.",
                    totals.deletes,
                ),
                Pending::ApplyPlan,
                Pending::Nothing,
            ));
            return;
        }

        self.apply_plan();
    }

    /// Hand the plan to the transfer queue and show the progress screen.
    ///
    /// Returns immediately. The main loop drives [`Transfer::step`] between
    /// frames, which is what makes the progress bar move and `esc` work --
    /// phase 3 applied the whole plan in one blocking call and the UI was
    /// frozen for the duration.
    fn apply_plan(&mut self) {
        let Some(plan) = self.plan.take() else { return };

        let settings = TransferSettings::from_config(&self.config);
        let endpoints = self.external_endpoints();
        let preference = self.active_executor();

        let (tool, unusable) = delegate::choose(preference, &plan, &endpoints);

        // A *named* executor that cannot run says so. `auto` says nothing,
        // because falling back to the native engine is what auto means --
        // whereas `executor = "rsync"` silently doing something else is how a
        // bandwidth-limited link ends up saturated.
        if let Some(why) = unusable {
            self.status = format!("{} not used: {why}", preference.label());
        }

        self.transfer_engine = tool.map_or("native", delegate::Tool::label);

        self.transfer = Some(match tool {
            Some(tool) => {
                match Transfer::delegated(plan.clone(), settings.clone(), tool, &endpoints, &[]) {
                    Ok(transfer) => {
                        if self.status.is_empty() {
                            self.status = format!("transferring with {}", tool.label());
                        }
                        transfer
                    }
                    Err(e) => {
                        // Spawning is the only part that can fail before any
                        // bytes move, so falling back loses nothing.
                        self.status = format!(
                            "{} could not start ({e}); using the native engine",
                            tool.label()
                        );
                        self.transfer_engine = "native";
                        Transfer::new(plan, settings)
                    }
                }
            }
            None => Transfer::new(plan, settings),
        });

        self.screen = Screen::Transfer;
    }

    /// Open the audit log.
    fn open_log(&mut self) {
        // Read fresh. `lazysync sync` from cron appends to the same file, so
        // a copy cached at startup would be missing exactly the runs nobody
        // watched -- which are the ones worth looking at.
        self.audit_entries = crate::sync::audit::recent(500);
        self.log_cursor = 0;
        self.screen = Screen::Log;
        self.status = if self.audit_entries.is_empty() {
            "nothing recorded yet".into()
        } else {
            format!("{} run(s) recorded", self.audit_entries.len())
        };
    }

    /// The audit log's keys.
    fn handle_log(&mut self, action: Action) {
        let last = self.audit_entries.len().saturating_sub(1);
        match action {
            Action::MoveDown => self.log_cursor = (self.log_cursor + 1).min(last),
            Action::MoveUp => self.log_cursor = self.log_cursor.saturating_sub(1),
            Action::MoveTop => self.log_cursor = 0,
            Action::MoveBottom => self.log_cursor = last,
            Action::HalfPageDown | Action::PageDown => {
                self.log_cursor = (self.log_cursor + 10).min(last);
            }
            Action::HalfPageUp | Action::PageUp => {
                self.log_cursor = self.log_cursor.saturating_sub(10);
            }
            Action::Cancel => {
                self.screen = Screen::Browser;
                self.status.clear();
            }
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            _ => {}
        }
    }

    /// Turn watch mode on or off for the local pane's tree.
    fn toggle_watch(&mut self) {
        if self.watcher.is_some() {
            self.watcher = None;
            self.status = "watching stopped".into();
            return;
        }

        // No profile and a profile that says `watch = "off"` are different
        // situations with different fixes, and saying "this profile" when
        // there is no profile sends somebody looking for a setting they have
        // not written yet.
        let Some(name) = self.active_profile.clone() else {
            self.status = "watching needs a sync profile on this connection; set one in its \
                           editor, with watch = \"batch\" or \"auto\""
                .into();
            return;
        };

        let mode = self.watch_mode();
        if matches!(mode, Watch::Off) {
            // A profile that does not ask to be watched is not watched by a
            // keystroke either: `watch` is where the behaviour is declared,
            // and `auto` in particular applies without confirming.
            self.status = format!(
                "[profiles.{name}] has watch = \"off\"; set \"batch\" or \"auto\" to use it"
            );
            return;
        }

        let root = self.panes[0].path().to_path_buf();
        match watch::Watcher::start(&root, self.watch_debounce()) {
            Ok(watcher) => {
                self.watcher = Some(watcher);
                self.status = format!(
                    "watching {} -- changes {} after {}ms of quiet",
                    root.display(),
                    match mode {
                        Watch::Auto => "sync automatically",
                        _ => "open a plan",
                    },
                    self.watch_debounce().as_millis()
                );
            }
            Err(e) => self.status = e,
        }
    }

    /// Whether this connection has a sync profile that exists in config.
    ///
    /// The two profile-driven commands are the only ones in the browser whose
    /// availability depends on the *connection* rather than on what is bound,
    /// so the key bar asks before advertising them -- a hint for a key that
    /// can only produce an error message is worse than no hint.
    pub fn profile_available(&self) -> bool {
        self.active_profile
            .as_ref()
            .is_some_and(|name| self.config.profiles.contains_key(name))
    }

    /// Whether watching would start, or is already running.
    pub fn watch_available(&self) -> bool {
        self.watcher.is_some() || !matches!(self.watch_mode(), Watch::Off)
    }

    /// What the active profile says about watching.
    fn watch_mode(&self) -> Watch {
        self.active_profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
            .map_or(Watch::Off, |profile| profile.watch)
    }

    fn watch_debounce(&self) -> std::time::Duration {
        let ms = self
            .active_profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
            .map_or(2000, |profile| profile.debounce_ms);
        std::time::Duration::from_millis(ms)
    }

    /// How long the main loop may block before the watcher needs attention.
    pub fn watch_deadline(&self) -> Option<std::time::Duration> {
        self.watcher.as_ref()?.next_deadline()
    }

    /// Give the watcher a chance to notice changes, and act on a ready batch.
    ///
    /// Called once per frame. Does nothing unless a batch has gone quiet for
    /// the debounce window.
    pub fn step_watch(&mut self) {
        // Not while something else is already in front: opening a plan over a
        // plan, or over a transfer, would take the screen out from under a
        // decision the user is in the middle of making.
        if !matches!(self.screen, Screen::Browser) || self.transfer.is_some() {
            return;
        }

        let Some(watcher) = &mut self.watcher else {
            return;
        };
        if !watcher.poll() {
            return;
        }

        let batch = watcher.take();
        let count = batch.len();

        // The batch decides *when* to sync, not what. The planner walks the
        // trees as it always does -- syncing only the paths that fired events
        // would miss whatever the watcher dropped (inotify queues overflow)
        // and would make watch mode disagree with what `sp` does on the same
        // trees.
        let mode = self.watch_mode();
        let profile_mode = self
            .active_profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
            .map_or(SyncMode::Push, |profile| profile.mode);

        self.begin_sync(profile_mode);

        let Some(plan) = &self.plan else {
            // Nothing to do after all: the change was to something excluded,
            // or it was already in agreement.
            self.status = format!("{count} change(s) seen, nothing to sync");
            return;
        };

        if plan.refusal.is_some() {
            // The safety rails apply in watch mode exactly as they do by
            // hand, and a refusal is the one thing that must never be
            // auto-applied.
            return;
        }

        match mode {
            Watch::Auto => {
                self.status = format!("{count} change(s): syncing");
                // Nobody saw this plan, which is exactly what the log needs
                // to record: an unattended sync is a different event from a
                // reviewed one.
                self.transfer_confirmed = false;
                self.apply_plan();
            }
            // Batch and Off both leave the plan on screen. `Off` cannot reach
            // here, since the watcher would not have started.
            _ => {
                self.status = format!("{count} change(s) since the last sync -- review and y");
            }
        }
    }

    /// Which executor the active profile asks for.
    fn active_executor(&self) -> Executor {
        self.active_profile
            .as_ref()
            .and_then(|name| self.config.profiles.get(name))
            .map_or(Executor::Auto, |profile| profile.executor)
    }

    /// How each pane's current directory looks to an external tool.
    fn external_endpoints(&self) -> delegate::Endpoints {
        delegate::Endpoints {
            local: self.panes[0].vfs.external_spec(self.panes[0].path()),
            remote: self.panes[1].vfs.external_spec(self.panes[1].path()),
        }
    }

    /// Do a slice of the transfer in flight. Called once per frame.
    pub fn step_transfer(&mut self) {
        let Some(transfer) = &mut self.transfer else {
            return;
        };
        if transfer.is_finished() {
            return;
        }

        // Handed over as panes, in pane order, rather than sorted into a
        // source and a destination: each row names the side it writes to, so
        // a bisync plan moving files both ways works through the same call.
        // The mode's direction is deliberately not consulted here any more --
        // it is already baked into every row's `target_pane`, and reading it
        // again would be a second source of truth about which way files go.
        let local_root = self.panes[0].path().to_path_buf();
        let remote_root = self.panes[1].path().to_path_buf();

        let (first, second) = self.panes.split_at_mut(1);

        let finished = transfer.step(
            first[0].vfs.as_mut(),
            &local_root,
            second[0].vfs.as_mut(),
            &remote_root,
        );

        if finished {
            self.finish_transfer();
        }
    }

    fn finish_transfer(&mut self) {
        let Some(transfer) = &self.transfer else {
            return;
        };

        for failure in transfer.report().failures() {
            tracing::warn!("{}: {:?}", failure.path.display(), failure.outcome);
        }
        self.status = transfer.report().summary();
        self.last_report = Some(transfer.report().clone());

        // Both panes get re-read: the destination obviously changed, and on a
        // local-to-local sync the source directory may have had a partial
        // file written next to it.
        for pane in &mut self.panes {
            pane.reload();
        }

        // Record what a bisync achieved, so the next run can tell a creation
        // from a deletion. Done after the panes reload, because the snapshot
        // re-stats what actually landed.
        //
        // The plan comes back off the *queue*, not from `self.plan`: applying
        // moves it into the queue, so reading `self.plan` here found nothing
        // and the snapshot was silently never written -- which made every
        // bisync a first run for ever, and deletions never propagate.
        let plan = self.transfer.as_ref().map(|t| t.plan().clone());
        if let (Some((local_root, remote_key)), Some(plan)) =
            (self.bisync_snapshot.clone(), plan.clone())
        {
            self.store_snapshot(&local_root, &remote_key, &plan);
            self.bisync_snapshot = None;
        }

        self.record_run(plan.as_ref());
    }

    /// Put the finished run in the audit log.
    ///
    /// Best effort by design: the sync has already happened, so refusing to
    /// acknowledge it because the record could not be written would be the
    /// wrong way round. A failure goes in the status line and nowhere else.
    fn record_run(&mut self, plan: Option<&Plan>) {
        let Some(report) = self.last_report.clone() else {
            return;
        };
        let Some(plan) = plan else { return };

        let entry = audit::Entry::from_report(
            chrono::Utc::now(),
            self.plan_mode.label(),
            self.transfer_engine,
            &plan.source_root.display().to_string(),
            &plan.dest_root.display().to_string(),
            self.transfer_confirmed,
            &report,
        );

        if let Err(e) = audit::append(&entry) {
            tracing::warn!("the audit log could not be written: {e}");
        }

        // Back to the default for the next run, which is reviewed unless
        // something says otherwise.
        self.transfer_confirmed = true;
    }

    /// Whether the main loop should keep stepping rather than blocking on
    /// input.
    pub fn transfer_is_active(&self) -> bool {
        self.transfer
            .as_ref()
            .is_some_and(|t| !t.is_finished() && t.state() != TransferState::Paused)
    }

    /// The progress screen's keys.
    fn handle_queue(&mut self, action: Action) {
        let Some(transfer) = &mut self.transfer else {
            self.screen = Screen::Browser;
            return;
        };

        // Once it is done, any key returns to the browser -- the screen is a
        // report at that point, not a control panel.
        if transfer.is_finished() {
            self.transfer = None;
            self.screen = Screen::Browser;
            return;
        }

        match action {
            Action::TogglePause => transfer.toggle_pause(),
            Action::Cancel => transfer.cancel(),
            Action::Help => self.overlay = Overlay::Help { scroll: 0 },
            _ => {}
        }
    }

    /// The yes/no dialog.
    ///
    /// Three ways to answer, which is deliberate: the selection plus Submit
    /// for somebody reading the buttons, and a direct key per answer for
    /// somebody who already knows what they are confirming. What none of them
    /// is, is a bare Enter meaning yes -- the selection starts on "no", so the
    /// reflex keystroke declines.
    fn handle_question(&mut self, action: Action) {
        let Overlay::Question(question) = &mut self.overlay else {
            return;
        };

        let pending = match action {
            // Two buttons, so either direction is the same move.
            Action::NextChoice | Action::PrevChoice => {
                question.answer = question.answer.other();
                return;
            }
            Action::Submit => match question.answer {
                Answer::Yes => question.on_yes.clone(),
                Answer::No => question.on_no.clone(),
            },
            Action::Apply => question.on_yes.clone(),
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

            Pending::DeleteBookmark { name } => {
                self.store.remove(&name);
                // Take the stored password with it, so deleting a connection
                // does not leave its secret orphaned in the keyring.
                if let Err(e) = secrets::forget(&name) {
                    tracing::debug!("could not forget the password for {name}: {e}");
                }
                match self.store.save_default() {
                    Ok(()) => self.status = format!("deleted {name}"),
                    Err(e) => self.status = format!("could not save: {e}"),
                }
                self.clamp_bookmark_cursor();
            }

            Pending::TrustHostKey => {
                let (Some(key), Some(bookmark)) = (self.host_key.take(), self.connecting.clone())
                else {
                    self.status = "nothing left to trust".into();
                    return;
                };

                match ssh::trust(&key) {
                    Ok(path) => {
                        tracing::info!("recorded a host key in {}", path.display());
                        self.collect_secret_and_open(&bookmark);
                    }
                    Err(e) => {
                        // Not fatal to the connection, only to remembering it:
                        // refusing to continue would be stricter than ssh
                        // itself, which connects and warns.
                        self.status = format!("could not record the host key: {e}");
                        self.collect_secret_and_open(&bookmark);
                    }
                }
            }

            Pending::ApplyPlan => self.apply_plan(),

            Pending::LeaveConnection => self.close_connection(),

            Pending::Nothing => {
                self.connecting = None;
                self.host_key = None;
                // Declining the delete confirmation returns to the window
                // rather than discarding the plan: the user may want to
                // exclude the deletions and apply the rest.
                if !matches!(self.screen, Screen::Confirm) {
                    self.status.clear();
                }
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
                self.status = "opening files is not implemented yet".into();
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
            Followed::Moved(_) => self.status.clear(),
            // Said plainly, because the panes are deliberately not moving
            // together and that would otherwise look like the feature had
            // broken. The second half is the part that matters: it tells you
            // the pairing is still there and you do not have to reopen the
            // connection to get it back.
            Followed::OutsideRoot => {
                self.status = "above the paired root -- go back in to re-couple".into();
            }
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
                self.overlay = Overlay::Question(Question::new(
                    format!("{} does not exist here. Create it?", path.display()),
                    Pending::CreateDir {
                        pane: follower,
                        path: path.clone(),
                    },
                    Pending::GhostAt {
                        pane: follower,
                        path,
                    },
                ));
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

/// How loudly the status line should say something.
///
/// It exists because a key press that produces only a message looked exactly
/// like the furniture beside it. Pressing sync and getting "nothing to push"
/// was reported as *nothing happening*, with the message on screen at the
/// time: it rendered in the ordinary foreground colour, immediately after
/// the mode badge, the path and the counters, all of which are also ordinary
/// foreground. An answer that is indistinguishable from a label is not an
/// answer anybody reads.
///
/// So the line now distinguishes what it is doing. Three levels is enough --
/// finer grades would be a palette nobody can learn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusKind {
    /// Ambient: where you are, what mode is on. True whether or not you did
    /// anything, and safe to ignore.
    #[default]
    Note,
    /// The direct answer to the key just pressed. Highlighted, because this
    /// is the case where silence reads as a broken binding.
    Answer,
    /// Something was refused or failed, and nothing happened as a result.
    Problem,
}

/// What to say when a plan came back empty.
///
/// Three things have to be in it, and the old message had none of them.
///
/// **What was examined.** "The trees already match" is a claim about the
/// whole directory, and when marks scoped the plan it is simply untrue --
/// nothing outside the marks was looked at. A message that overstates its
/// own coverage is how somebody concludes a file is up to date when it was
/// never compared.
///
/// **How many.** One marked file reads differently from nine, and the
/// singular is the common case: you mark the thing you just edited.
///
/// **What "match" actually meant.** This is the one that prompted the
/// change. Over FTP the comparison degrades to size alone -- the protocol
/// reports timestamps but cannot set them, so comparing them would mark
/// every uploaded file as changed on the next run. An edit that leaves the
/// byte count the same is therefore invisible, and being told "already in
/// sync" with no mention of that is the application sounding certain about
/// something it did not check. So the rule that was actually applied is
/// named whenever it is not the one the config asked for.
fn nothing_to_do(
    mode: SyncMode,
    scoped_names: &[String],
    method: crate::sync::compare::Method,
    config: &Config,
) -> String {
    let what = match scoped_names {
        [] => format!("nothing to {} -- the trees already match", mode.label()),
        [one] => format!("{one} is already in sync"),
        many => format!("all {} marked items are already in sync", many.len()),
    };

    // Only when the rule differs from the one requested. Announcing
    // "compared by size+mtime" on every no-op would be noise, and noise is
    // what stops the one that matters from being read.
    use crate::{config::CompareMethod, sync::compare::Method};
    let requested = match config.compare.method {
        CompareMethod::SizeMtime => Method::SizeMtime,
        CompareMethod::SizeOnly => Method::SizeOnly,
        CompareMethod::Checksum => Method::Checksum,
    };
    if method == requested {
        return what;
    }
    format!("{what} -- compared by {} only", method.describe())
}

/// Which side a pane index refers to, for titles.
pub const fn side_label(index: usize) -> &'static str {
    if index == PaneSide::Local.index() {
        "local"
    } else {
        "remote"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::CompareMethod, sync::compare::Method};

    fn config_with(method: CompareMethod) -> Config {
        let mut config = Config::default();
        config.compare.method = method;
        config
    }

    /// One marked file names itself. This is the common case -- you mark the
    /// thing you just edited -- and "the trees already match" was both
    /// plural and a claim about files that were never examined.
    #[test]
    fn one_marked_item_is_named() {
        let message = nothing_to_do(
            SyncMode::Push,
            &["about-us.php".to_string()],
            Method::SizeMtime,
            &config_with(CompareMethod::SizeMtime),
        );
        assert_eq!(message, "about-us.php is already in sync");
    }

    #[test]
    fn several_marked_items_are_counted() {
        let names = [
            "a.php".to_string(),
            "b.php".to_string(),
            "c.php".to_string(),
        ];
        let message = nothing_to_do(
            SyncMode::Push,
            &names,
            Method::SizeMtime,
            &config_with(CompareMethod::SizeMtime),
        );
        assert_eq!(message, "all 3 marked items are already in sync");
    }

    /// Nothing marked is a statement about the whole directory, and may say
    /// so.
    #[test]
    fn an_unscoped_plan_still_talks_about_the_trees() {
        let message = nothing_to_do(
            SyncMode::Push,
            &[],
            Method::SizeMtime,
            &config_with(CompareMethod::SizeMtime),
        );
        assert_eq!(message, "nothing to push -- the trees already match");
    }

    /// The case that prompted all of this. Over FTP the comparison degrades
    /// to size alone, so an edit that leaves the byte count unchanged is
    /// invisible -- and saying "already in sync" without mentioning that is
    /// the application sounding certain about something it did not check.
    #[test]
    fn a_degraded_comparison_says_what_it_actually_compared() {
        let message = nothing_to_do(
            SyncMode::Push,
            &["about-us.php".to_string()],
            Method::SizeOnly,
            &config_with(CompareMethod::SizeMtime),
        );
        assert_eq!(
            message,
            "about-us.php is already in sync -- compared by size only"
        );
    }

    /// But a comparison that did what was asked does not editorialise. A
    /// caveat printed every time is a caveat nobody reads.
    #[test]
    fn an_undegraded_comparison_adds_no_caveat() {
        let message = nothing_to_do(
            SyncMode::Push,
            &["x".to_string()],
            Method::SizeOnly,
            &config_with(CompareMethod::SizeOnly),
        );
        assert_eq!(message, "x is already in sync");
    }
}
