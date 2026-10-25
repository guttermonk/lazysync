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
    config::{Config, Palette, PaneSide, SyncMode},
    connection::Secret,
    keys::{Action, Keymaps, Mode},
    sync::{
        diff,
        filters::Filters,
        plan::{Plan, Section},
        safety::{self, DeletePolicy},
        transfer::{State as TransferState, Transfer},
    },
    ui::{confirm, form::ConnectionForm},
    vfs::{ftp, hostkey::HostKey, ssh, LocalFs, Vfs},
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

    /// The transfer in flight, stepped by the main loop between frames.
    pub transfer: Option<Transfer>,

    /// Name of the sync profile the open connection references, if any.
    ///
    /// Set when a bookmark opens. It decides the exclude rules and the
    /// default direction, so a `[profiles.deploy]` entry is not just
    /// documentation for the headless path.
    pub active_profile: Option<String>,

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
        let store = BookmarkStore::load_default().unwrap_or_else(|e| {
            tracing::warn!("ignoring unreadable bookmarks: {e}");
            BookmarkStore::default()
        });

        let mut app = Self {
            focus: config.default_pane.index(),
            sync_browse: config.sync_browse.enabled,
            union_view: config.sync_browse.union_view,
            union_cursor: 0,
            desynced: false,
            overlay: Overlay::None,
            status: String::new(),
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
            transfer: None,
            active_profile: None,
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
            // A yes/no question reuses the confirm keymap, so `y`/`n` mean the
            // same thing here as they will in the sync confirmation window.
            Overlay::Question(_) => return Mode::Confirm,
            Overlay::None => {}
        }

        match self.screen {
            Screen::Bookmarks => Mode::Bookmarks,
            Screen::Browser => Mode::Browser,
            Screen::Form => Mode::Form,
            Screen::Confirm => Mode::Confirm,
            Screen::Transfer => Mode::Queue,
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
            Mode::Bookmarks => self.handle_bookmarks(action),
            Mode::Browser => self.handle_browser(action),
            Mode::Form => self.handle_form(action),
            Mode::Help => self.handle_help(action),
            Mode::Prompt => self.handle_prompt(action),
            Mode::Confirm => self.handle_confirm(action),
            Mode::Queue => self.handle_queue(action),
            // The log viewer arrives with the audit log it shows.
            Mode::Log => {}
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
            Action::EditItem => match self.selected_bookmark() {
                Some(bookmark) => {
                    self.form = Some(ConnectionForm::edit(bookmark));
                    self.screen = Screen::Form;
                    self.status.clear();
                }
                None => self.status = "nothing to edit".into(),
            },
            Action::DeleteItem => match self.selected_bookmark() {
                Some(bookmark) => {
                    let name = bookmark.name.clone();
                    self.overlay = Overlay::Question(Question {
                        text: format!("Delete the connection {name:?}?"),
                        on_yes: Pending::DeleteBookmark { name },
                        on_no: Pending::Nothing,
                    });
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
                if self.bookmark_filter.is_empty() {
                    // Nothing to back out of. Leaving the list with no filter
                    // set means going to the local browser, which is still a
                    // useful thing to do with no connections at all.
                    self.screen = Screen::Browser;
                    self.status = "local browser".into();
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

        if !target.protocol.is_implemented() {
            self.status = format!(
                "{} arrives in phase {} -- see docs/PLAN.md",
                target.protocol,
                target.protocol.arrives_in_phase()
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
                self.overlay = Overlay::Question(Question {
                    text: format!(
                        "{host_port} is not in known_hosts. Fingerprint {fingerprint}. \
                         Check it against the server before trusting it. Trust and continue?"
                    ),
                    on_yes: Pending::TrustHostKey,
                    on_no: Pending::Nothing,
                });
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
        let opened = if target.protocol.is_ssh() {
            ssh::open(&target, secret)
        } else {
            ftp::open(&target, secret)
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
        mut fs: crate::vfs::RemoteAdapter,
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
        self.panes[1] = Pane::new(Box::new(fs), start, &self.config);

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
            self.status = format!("{} arrives in phase 6 -- see docs/PLAN.md", mode.label());
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

        // Split the array so both panes can be borrowed mutably at once.
        let (first, second) = self.panes.split_at_mut(1);
        let (source, dest) = if source_index == 0 {
            (&mut first[0], &mut second[0])
        } else {
            (&mut second[0], &mut first[0])
        };

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
        );

        safety::check(&mut plan, &self.config.safety);

        if plan.is_empty() && plan.unreadable.is_empty() {
            self.status = format!("nothing to {} -- the trees already match", mode.label());
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

    /// Plan using the active profile's own mode.
    ///
    /// The point of a profile is not having to remember which direction a
    /// given connection syncs in.
    pub fn begin_profile_sync(&mut self) {
        let Some(name) = self.active_profile.clone() else {
            self.status = "this connection has no sync profile; use sp, sl or sm".into();
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
        // A yes/no question overlay also uses this keymap; it is handled
        // separately and takes priority.
        if matches!(self.overlay, Overlay::Question(_)) {
            self.handle_question(action);
            return;
        }

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

            Action::Checksum => {
                self.status =
                    "re-checking by checksum arrives in phase 4, with the transfer queue".into();
            }
            Action::DiffRow => {
                self.status = "inline diffs arrive in phase 4".into();
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
            self.overlay = Overlay::Question(Question {
                text: format!(
                    "Permanently delete {} file(s) on the {} side? This cannot be undone.",
                    totals.deletes,
                    side_label(self.plan_mode.direction().dest())
                ),
                on_yes: Pending::ApplyPlan,
                on_no: Pending::Nothing,
            });
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

        self.transfer = Some(Transfer::new(
            plan,
            DeletePolicy::from_config(&self.config.safety),
            self.config.transfer.partial_suffix.clone(),
            self.config.transfer.resume,
        ));
        self.screen = Screen::Transfer;
        self.status.clear();
    }

    /// Do a slice of the transfer in flight. Called once per frame.
    pub fn step_transfer(&mut self) {
        let Some(transfer) = &mut self.transfer else {
            return;
        };
        if transfer.is_finished() {
            return;
        }

        let direction = self.plan_mode.direction();
        let source_index = direction.source;
        let dest_index = direction.dest();

        let source_root = self.panes[source_index].path().to_path_buf();
        let dest_root = self.panes[dest_index].path().to_path_buf();

        let (first, second) = self.panes.split_at_mut(1);
        let (source, dest) = if source_index == 0 {
            (&mut first[0], &mut second[0])
        } else {
            (&mut second[0], &mut first[0])
        };

        let finished = transfer.step(
            source.vfs.as_mut(),
            &source_root,
            dest.vfs.as_mut(),
            &dest_root,
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

        // Both panes get re-read: the destination obviously changed, and on a
        // local-to-local sync the source directory may have had a partial
        // file written next to it.
        for pane in &mut self.panes {
            pane.reload();
        }
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
