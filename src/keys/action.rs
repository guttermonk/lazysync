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
    /// The yes/no dialog.
    ///
    /// Its own mode rather than a borrowed `Confirm` one, because the two
    /// windows disagree about what a key means: `n` in the confirmation
    /// window is a motion in anybody's config that rebinds the layout, while
    /// in a two-button dialog it is the answer "no". Sharing the map made the
    /// dialog's own footer wrong for exactly the users who had rebound
    /// something -- see `ui::question`.
    Question,
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
    pub const ALL: [Self; 9] = [
        Self::Bookmarks,
        Self::Browser,
        Self::Confirm,
        Self::Question,
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
            Self::Question => "question",
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

    // ── File operations ──────────────────────────────────────────────────
    //
    // Distinct from the `*Item` actions below, which act on a *connection* in
    // the bookmark list. Sharing names would mean one binding meaning "delete
    // this saved host" in one mode and "delete this file" in another, which
    // is the sort of thing a `base` keymap gets wrong once and loudly.
    /// Create an empty file in the focused pane.
    NewFile,
    /// Create a directory in the focused pane.
    NewDir,
    /// Rename the selected entry.
    Rename,
    /// Delete the marked entries, or the selected one.
    DeleteFile,
    /// Change the Unix permission bits of the selected entry.
    Chmod,
    /// Open the selected file in `$EDITOR`, and write it back if it changed.
    ///
    /// Unlike [`Self::OpenExternal`], which hands a file to the desktop and
    /// forgets it, this one owns the terminal for the duration and is a
    /// round trip: a remote file is fetched, edited locally, and uploaded
    /// again only if the editor actually changed it.
    EditFile,

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
    /// Hand the selected file to the system's default application.
    ///
    /// Local files only, and deliberately: opening a remote one means
    /// downloading it to a temporary path first, and a file manager that
    /// silently leaves copies of a server's files in `/tmp` is one you
    /// cannot audit. It says so rather than guessing.
    ///
    /// The handler is launched detached with no terminal of its own, so this
    /// is for GUI applications. A terminal-based handler would otherwise be
    /// drawing into the same cells lazysync is; see `app::App::open_external`.
    OpenExternal,
    Submit,
    Cancel,
    Help,
    /// Put the window away without closing anything.
    ///
    /// lazysync has no window of its own to hide -- it is a terminal program,
    /// and the only thing that can hide it is whatever opened it. So this
    /// runs `[zide] hide_command` and keeps running: the connection, the
    /// marks, the cursor and a transfer in flight are all still there when
    /// the popup comes back, which is the difference between this and
    /// [`Self::Quit`].
    ///
    /// Unbound by default, and deliberately. The command it needs is empty by
    /// default, because lazysync cannot know what is hosting it, and a key
    /// that is bound to a no-op is worse than one that is not bound at all --
    /// the first reads as a broken feature, the second sends you to `?`.
    Hide,
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

            Self::SyncPush => "One-way local to remote; deletes nothing",
            Self::SyncPull => "One-way remote to local; deletes nothing",
            Self::SyncMirror => "One-way: remote ends up identical to local",
            Self::SyncProfile => "Sync as this connection's profile says",
            Self::SyncBisync => "Two-way: each change travels to the other side",
            Self::TogglePause => "Pause or resume the transfer",
            Self::ToggleWatch => "Watch and sync on change (needs a profile)",
            Self::ShowLog => "Show the audit log",
            Self::ShowConnections => "Back to the connection list",

            Self::NewFile => "Create an empty file",
            Self::NewDir => "Create a directory",
            Self::Rename => "Rename the selected entry",
            Self::DeleteFile => "Delete the marked entries, or the selected one",
            Self::Chmod => "Change permissions on the selected entry",
            Self::EditFile => "Edit the selected file in $EDITOR",

            Self::OpenItem => "Open the selected connection",
            Self::NewItem => "Add a new connection",
            Self::EditItem => "Edit this connection",
            Self::DeleteItem => "Delete this connection",
            Self::NextChoice => "Next value",
            Self::PrevChoice => "Previous value",

            Self::OpenExternal => "Open the file in the system's default application",
            Self::Submit => "Accept",
            Self::Cancel => "Cancel, or close the overlay",
            Self::Help => "Show this help",
            Self::Hide => "Hide the window; the connection stays open",
            Self::Quit => "Quit lazysync",
        }
    }

    /// Two or three words, for a key hint.
    ///
    /// [`Self::describe`] is a sentence for the help overlay; this is the
    /// label that fits on a bar next to a key. They are kept apart rather
    /// than one truncating the other, because "One-way local to remote;
    /// delet" is not a shorter hint, it is a worse one.
    pub const fn label(self) -> &'static str {
        match self {
            Self::MoveDown => "down",
            Self::MoveUp => "up",
            Self::MoveTop => "to top",
            Self::MoveBottom => "to bottom",
            Self::HalfPageDown => "half page down",
            Self::HalfPageUp => "half page up",
            Self::PageDown => "page down",
            Self::PageUp => "page up",

            Self::EnterDir => "enter",
            Self::ParentDir => "parent",
            Self::GoHome => "home",
            Self::GoRoot => "root",
            Self::Refresh => "reload",

            Self::NextPane => "other pane",
            Self::PrevPane => "previous pane",
            Self::SyncOtherPaneHere => "bring other pane here",

            Self::ToggleHidden => "dotfiles",
            Self::ToggleSyncBrowse => "sync-browse",
            Self::ToggleUnionView => "union view",
            Self::ToggleDirSizes => "dir sizes",
            Self::CycleSort => "sort",
            Self::ReverseSort => "reverse sort",

            Self::ToggleMark => "mark",
            Self::MarkAll => "mark all",
            Self::ClearMarks => "clear marks",
            Self::InvertMarks => "invert marks",

            Self::Search => "find",
            Self::SearchNext => "next match",
            Self::SearchPrev => "previous match",

            Self::Apply => "apply",
            Self::ToggleRow => "include",
            Self::InvertRows => "invert",
            Self::AllRows => "all",
            Self::OnlyRow => "only this",
            Self::Checksum => "checksum",
            Self::DiffRow => "diff",
            Self::FoldSection => "fold",
            Self::UnfoldSection => "unfold",
            Self::ResolveKeepLocal => "keep local",
            Self::ResolveKeepRemote => "keep remote",
            Self::ResolveKeepBoth => "keep both",
            Self::ResolveSkip => "skip",

            Self::SyncPush => "push",
            Self::SyncPull => "pull",
            Self::SyncMirror => "mirror",
            Self::SyncProfile => "profile",
            Self::SyncBisync => "bisync",
            Self::TogglePause => "pause",
            Self::ToggleWatch => "watch",
            Self::ShowLog => "log",
            Self::ShowConnections => "connections",

            Self::NewFile => "new file",
            Self::NewDir => "new dir",
            Self::Rename => "rename",
            Self::DeleteFile => "delete",
            Self::Chmod => "permissions",
            Self::EditFile => "edit",

            Self::OpenItem => "open",
            Self::NewItem => "new",
            Self::EditItem => "edit",
            Self::DeleteItem => "delete",
            Self::NextChoice => "next value",
            Self::PrevChoice => "previous value",

            Self::OpenExternal => "open externally",
            Self::Submit => "accept",
            Self::Cancel => "cancel",
            Self::Help => "help",
            Self::Hide => "hide",
            Self::Quit => "quit",
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

            Self::NewFile
            | Self::NewDir
            | Self::Rename
            | Self::DeleteFile
            | Self::Chmod
            | Self::EditFile => "Files",

            Self::ShowConnections
            | Self::OpenItem
            | Self::NewItem
            | Self::EditItem
            | Self::DeleteItem
            | Self::NextChoice
            | Self::PrevChoice => "Connections",

            Self::OpenExternal
            | Self::Submit
            | Self::Cancel
            | Self::Help
            | Self::Hide
            | Self::Quit => "General",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Keymaps;
    use std::collections::BTreeMap;

    /// Every action the default keymaps bind, which is every action the
    /// which-key bar can be asked to name.
    fn bound_actions() -> Vec<Action> {
        let maps = Keymaps::resolve(&BTreeMap::new());
        let mut all: Vec<Action> = Vec::new();
        for mode in Mode::ALL {
            for action in maps.get(mode).actions() {
                if !all.contains(&action) {
                    all.push(action);
                }
            }
        }
        assert!(!all.is_empty(), "the defaults bind nothing?");
        all
    }

    /// A label has to fit on a bar beside its key. The ceiling is generous --
    /// this is a guard against someone pasting a sentence in, not a style
    /// rule -- but a `describe()` string would blow straight through it.
    #[test]
    fn every_label_is_short_enough_for_a_hint() {
        for action in bound_actions() {
            let label = action.label();
            assert!(!label.is_empty(), "{action} has no label");
            assert!(
                label.chars().count() <= 22,
                "{action}'s label is a sentence: {label:?}"
            );
            assert!(
                !label.ends_with('.'),
                "{action}'s label is punctuated like prose: {label:?}"
            );
        }
    }

    /// The two exist for different places, so one must not have been filled
    /// in by copying the other.
    #[test]
    fn a_label_is_not_the_description() {
        for action in bound_actions() {
            assert_ne!(
                action.label(),
                action.describe(),
                "{action}'s label is its whole description"
            );
        }
    }

    /// `Hide` ships with no key, so [`bound_actions`] never reaches it and
    /// the two checks above cannot. It is still namable the moment somebody
    /// binds it, which is the only state a user ever sees it in.
    #[test]
    fn an_action_with_no_default_key_is_still_describable() {
        assert!(!bound_actions().contains(&Action::Hide));

        assert_eq!(Action::Hide.label(), "hide");
        assert_ne!(Action::Hide.label(), Action::Hide.describe());
        assert_eq!(Action::Hide.group(), "General");
        assert_eq!(Action::Hide.name(), "Hide", "the spelling config accepts");
    }

    /// The bar reads `sp push`, so the label must not repeat the key or the
    /// action's own name -- `sp SyncPush` is noise.
    #[test]
    fn the_sync_chords_read_as_words() {
        assert_eq!(Action::SyncPush.label(), "push");
        assert_eq!(Action::SyncBisync.label(), "bisync");
    }
}
