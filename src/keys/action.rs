//! What a key can do, and where.
//!
//! One `Action` enum spans every mode rather than one enum per mode, so that
//! an action name in a config file means the same thing wherever it appears
//! and `lazysync keys` can list them all. What differs per mode is the
//! *keymap*: `d` is [`Action::DiffRow`] in the confirmation window and part of
//! a delete chord in the browser.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Which keymap is consulted for a key press.
///
/// The active mode is derived from what is on screen, not stored as state you
/// can forget to update -- see `App::mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The bookmark list, which is the screen lazysync opens on.
    Bookmarks,
    /// The dual-pane file browser.
    Browser,
    /// The sync confirmation window.
    Confirm,
    /// A multi-field form: connection details, bookmark editor.
    Form,
    /// A single-line prompt: search, rename, password.
    Prompt,
    /// The help overlay.
    Help,
    /// The transfer queue.
    Queue,
    /// The transfer log viewer.
    Log,
}

impl Mode {
    /// Every mode, in the order `lazysync keys` prints them.
    pub const ALL: [Self; 8] = [
        Self::Bookmarks,
        Self::Browser,
        Self::Confirm,
        Self::Form,
        Self::Prompt,
        Self::Help,
        Self::Queue,
        Self::Log,
    ];

    /// The name used in config (`[keys.browser]`) and on the CLI.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bookmarks => "bookmarks",
            Self::Browser => "browser",
            Self::Confirm => "confirm",
            Self::Form => "form",
            Self::Prompt => "prompt",
            Self::Help => "help",
            Self::Queue => "queue",
            Self::Log => "log",
        }
    }

    /// Parse a mode name, for `lazysync keys --mode`.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }

    /// In a prompt or a form, an unbound printable key is text to insert
    /// rather than an unknown command to report.
    pub const fn takes_text_input(self) -> bool {
        matches!(self, Self::Prompt | Self::Form)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Everything a keybinding can be bound to.
///
/// Variants are serialized in `PascalCase`, which is how they appear in
/// config: `["ToggleSyncBrowse", "S"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    // ── Motion ───────────────────────────────────────────────────────────
    MoveDown,
    MoveUp,
    MoveTop,
    MoveBottom,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,

    // ── Navigation ───────────────────────────────────────────────────────
    EnterDir,
    ParentDir,
    GoHome,
    GoRoot,
    Refresh,

    // ── Panes ────────────────────────────────────────────────────────────
    NextPane,
    PrevPane,
    /// Move the other pane to this pane's directory, once, without turning
    /// synchronized browsing on.
    SyncOtherPaneHere,

    // ── View ─────────────────────────────────────────────────────────────
    ToggleHidden,
    ToggleSyncBrowse,
    ToggleUnionView,
    ToggleDirSizes,
    CycleSort,
    ReverseSort,

    // ── Marks ────────────────────────────────────────────────────────────
    ToggleMark,
    MarkAll,
    ClearMarks,
    InvertMarks,

    // ── Search ───────────────────────────────────────────────────────────
    Search,
    SearchNext,
    SearchPrev,

    // ── Confirmation window ──────────────────────────────────────────────
    Apply,
    ToggleRow,
    InvertRows,
    AllRows,
    OnlyRow,
    Checksum,
    DiffRow,
    FoldSection,
    UnfoldSection,
    /// Settle the conflict under the cursor by keeping the local copy.
    ResolveKeepLocal,
    /// Settle it by keeping the remote copy.
    ResolveKeepRemote,
    /// Settle it by keeping both, the loser under a suffixed name.
    ResolveKeepBoth,
    /// Leave both sides alone this run, and be asked again next time.
    ResolveSkip,

    // ── Sync ─────────────────────────────────────────────────────────────
    /// Plan a push: local to remote, without deleting anything.
    SyncPush,
    /// Plan a pull: remote to local, without deleting anything.
    SyncPull,
    /// Plan a mirror: make the remote identical, deletions included.
    SyncMirror,
    /// Plan using the open connection's own sync profile.
    SyncProfile,
    /// Plan a two-way sync, using the stored snapshot to tell a creation on
    /// one side from a deletion on the other.
    SyncBisync,
    /// Pause or resume the transfer in flight.
    TogglePause,
    /// Start or stop watching the local tree and syncing what changes.
    ToggleWatch,
    /// Open the audit log: what previous runs actually did.
    ShowLog,
    /// Go back to the connection list.
    ///
    /// Esc on the connection list drops to the browser, and before this
    /// existed that door only opened one way: there was no binding anywhere
    /// that returned to the list, so the only way back was to quit.
    ShowConnections,

    // ── Bookmarks and forms ──────────────────────────────────────────────
    /// Open the selected bookmark.
    OpenItem,
    /// Add a new connection, on its own page.
    NewItem,
    /// Edit the selected connection -- the only place its details are shown.
    EditItem,
    DeleteItem,
    /// Next value of a choice field. Bound to the arrow keys rather than
    /// `h`/`l`, which in a form are literal characters.
    NextChoice,
    PrevChoice,

    // ── Generic ──────────────────────────────────────────────────────────
    Submit,
    Cancel,
    Help,
    Quit,
}

impl Action {
    /// One line for the help overlay. The overlay is built from the live
    /// keymap and these strings, so a binding can never be documented with a
    /// key it no longer has.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::MoveDown => "Down one entry",
            Self::MoveUp => "Up one entry",
            Self::MoveTop => "To the first entry",
            Self::MoveBottom => "To the last entry",
            Self::HalfPageDown => "Down half a screen",
            Self::HalfPageUp => "Up half a screen",
            Self::PageDown => "Down a screen",
            Self::PageUp => "Up a screen",

            Self::EnterDir => "Enter the selected directory",
            Self::ParentDir => "Up to the parent directory",
            Self::GoHome => "To your home directory",
            Self::GoRoot => "To the filesystem root",
            Self::Refresh => "Re-read the current directory",

            Self::NextPane => "Focus the other pane",
            Self::PrevPane => "Focus the previous pane",
            Self::SyncOtherPaneHere => "Move the other pane here, once",

            Self::ToggleHidden => "Show or hide dotfiles",
            Self::ToggleSyncBrowse => "Synchronized browsing on/off",
            Self::ToggleUnionView => "Merge both panes' listings",
            Self::ToggleDirSizes => "Recursive directory sizes on/off",
            Self::CycleSort => "Next sort key",
            Self::ReverseSort => "Reverse the sort order",

            Self::ToggleMark => "Mark or unmark the selected entry",
            Self::MarkAll => "Mark everything",
            Self::ClearMarks => "Clear all marks",
            Self::InvertMarks => "Invert the marks",

            Self::Search => "Search in this pane",
            Self::SearchNext => "Next match",
            Self::SearchPrev => "Previous match",

            Self::Apply => "Apply the plan",
            Self::ToggleRow => "Include or exclude this row",
            Self::InvertRows => "Invert which rows are included",
            Self::AllRows => "Include every row",
            Self::OnlyRow => "Include only this row",
            Self::Checksum => "Re-compare by checksum",
            Self::DiffRow => "Show a diff for this row",
            Self::FoldSection => "Fold this section",
            Self::UnfoldSection => "Unfold this section",
            Self::ResolveKeepLocal => "Conflict: keep the local copy",
            Self::ResolveKeepRemote => "Conflict: keep the remote copy",
            Self::ResolveKeepBoth => "Conflict: keep both, one suffixed",
            Self::ResolveSkip => "Conflict: leave both alone this run",

            Self::SyncPush => "Plan a push to the other pane",
            Self::SyncPull => "Plan a pull from the other pane",
            Self::SyncMirror => "Plan a mirror, deletions included",
            Self::SyncProfile => "Plan using this connection's sync profile",
            Self::SyncBisync => "Plan a two-way sync",
            Self::TogglePause => "Pause or resume the transfer",
            Self::ToggleWatch => "Watch this tree and sync changes",
            Self::ShowLog => "Show the audit log",
            Self::ShowConnections => "Back to the connection list",

            Self::OpenItem => "Open the selected connection",
            Self::NewItem => "Add a new connection",
            Self::EditItem => "Edit this connection",
            Self::DeleteItem => "Delete this connection",
            Self::NextChoice => "Next value",
            Self::PrevChoice => "Previous value",

            Self::Submit => "Accept",
            Self::Cancel => "Cancel, or close the overlay",
            Self::Help => "Show this help",
            Self::Quit => "Quit lazysync",
        }
    }

    /// Heading this action is listed under in the help overlay.
    pub const fn group(self) -> &'static str {
        match self {
            Self::MoveDown
            | Self::MoveUp
            | Self::MoveTop
            | Self::MoveBottom
            | Self::HalfPageDown
            | Self::HalfPageUp
            | Self::PageDown
            | Self::PageUp => "Motion",

            Self::EnterDir | Self::ParentDir | Self::GoHome | Self::GoRoot | Self::Refresh => {
                "Navigation"
            }

            Self::NextPane | Self::PrevPane | Self::SyncOtherPaneHere => "Panes",

            Self::ToggleHidden
            | Self::ToggleSyncBrowse
            | Self::ToggleUnionView
            | Self::ToggleDirSizes
            | Self::CycleSort
            | Self::ReverseSort => "View",

            Self::ToggleMark | Self::MarkAll | Self::ClearMarks | Self::InvertMarks => "Marks",

            Self::Search | Self::SearchNext | Self::SearchPrev => "Search",

            Self::Apply
            | Self::ToggleRow
            | Self::InvertRows
            | Self::AllRows
            | Self::OnlyRow
            | Self::Checksum
            | Self::DiffRow
            | Self::FoldSection
            | Self::UnfoldSection
            | Self::ResolveKeepLocal
            | Self::ResolveKeepRemote
            | Self::ResolveKeepBoth
            | Self::ResolveSkip => "Plan",

            Self::SyncPush
            | Self::SyncPull
            | Self::SyncMirror
            | Self::SyncProfile
            | Self::SyncBisync
            | Self::TogglePause
            | Self::ToggleWatch
            | Self::ShowLog => "Sync",

            Self::ShowConnections
            | Self::OpenItem
            | Self::NewItem
            | Self::EditItem
            | Self::DeleteItem
            | Self::NextChoice
            | Self::PrevChoice => "Connections",

            Self::Submit | Self::Cancel | Self::Help | Self::Quit => "General",
        }
    }

    /// The name used in config and printed by `lazysync keys`.
    ///
    /// `Debug` and serde's `PascalCase` produce the same spelling for a
    /// fieldless variant, so this stays in step with what the config parser
    /// accepts without a second table to keep in sync.
    pub fn name(self) -> String {
        format!("{self:?}")
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
