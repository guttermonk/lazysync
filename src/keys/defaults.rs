//! The built-in vim keymap.
//!
//! These are the bindings a `base` table replaces and an `extra` table layers
//! over. Where vim has an opinion, it wins: `j`/`k` move, `gg`/`G` jump,
//! `ctrl+d`/`ctrl+u` scroll by half screens, `/` searches, `n`/`N` cycle
//! matches, `ZZ` quits.
//!
//! Arrow keys and the navigation cluster are bound alongside the vim keys
//! rather than instead of them. Someone reaching for `PageDown` is not asking
//! to be taught vim.

use super::{
    action::{Action, Mode},
    combo::{Key, KeyCombo, ModifiedKey},
    keymap::Keymap,
};

/// The default keymap for one mode.
pub fn for_mode(mode: Mode) -> Keymap {
    match mode {
        Mode::Bookmarks => bookmarks(),
        Mode::Browser => browser(),
        Mode::Confirm => confirm(),
        Mode::Question => question(),
        Mode::Form => form(),
        Mode::Prompt => prompt(),
        Mode::Help => help(),
        Mode::Queue => queue(),
        Mode::Log => log(),
    }
}

// ── Builders, to keep the tables below readable ──────────────────────────────

/// A single unmodified key: `c('j')`.
fn c(ch: char) -> KeyCombo {
    KeyCombo::single(ModifiedKey::plain(Key::Char(ch)))
}

/// A single named key: `n(Key::Enter)`.
fn n(key: Key) -> KeyCombo {
    KeyCombo::single(ModifiedKey::plain(key))
}

/// Ctrl plus a character: `ctrl('d')`.
fn ctrl(ch: char) -> KeyCombo {
    KeyCombo::single(ModifiedKey::ctrl(ch))
}

/// Shift plus a named key: `shift(Key::Enter)`.
///
/// Only for the named keys. Shift plus a *character* is spelled as the
/// uppercase character -- see [`ModifiedKey::from_event`], which drops a
/// Shift the letter already carries.
fn shift(key: Key) -> KeyCombo {
    KeyCombo::single(ModifiedKey::new(key, super::combo::Mods::shift()))
}

/// A chord of unmodified characters: `chord("gg")`.
fn chord(keys: &str) -> KeyCombo {
    KeyCombo::chord(keys.chars().map(|ch| ModifiedKey::plain(Key::Char(ch))))
}

/// Motions shared by every scrollable mode, so a list behaves the same
/// wherever it appears.
fn motions() -> Vec<(Action, KeyCombo)> {
    vec![
        (Action::MoveDown, c('j')),
        (Action::MoveDown, n(Key::Down)),
        (Action::MoveUp, c('k')),
        (Action::MoveUp, n(Key::Up)),
        (Action::MoveTop, chord("gg")),
        (Action::MoveTop, n(Key::Home)),
        (Action::MoveBottom, c('G')),
        (Action::MoveBottom, n(Key::End)),
        (Action::HalfPageDown, ctrl('d')),
        (Action::HalfPageUp, ctrl('u')),
        (Action::PageDown, ctrl('f')),
        (Action::PageDown, n(Key::PageDown)),
        (Action::PageUp, ctrl('b')),
        (Action::PageUp, n(Key::PageUp)),
    ]
}

// ── Modes ───────────────────────────────────────────────────────────────────

fn browser() -> Keymap {
    let mut bindings = motions();
    bindings.extend([
        // Navigation. `h`/`l` as out/in is the file-manager convention, and
        // Backspace joins `h` because it is what muscle memory reaches for.
        (Action::EnterDir, c('l')),
        (Action::EnterDir, n(Key::Enter)),
        (Action::EnterDir, n(Key::Right)),
        (Action::ParentDir, c('h')),
        (Action::ParentDir, n(Key::Left)),
        (Action::ParentDir, n(Key::Backspace)),
        (Action::GoHome, chord("g~")),
        (Action::GoRoot, chord("g/")),
        (Action::Refresh, ctrl('r')),
        // Open in the system's handler. `shift+enter` is the asked-for key
        // and leads, but it cannot be the only one: most terminals send
        // Shift+Enter as a plain Enter, and only those speaking the kitty
        // keyboard protocol distinguish the two. A key that works on some
        // terminals and silently does something else on the rest is worse
        // than no key, so `gx` -- vim's "open this with the system handler"
        // -- is bound alongside it and works everywhere.
        (Action::OpenExternal, shift(Key::Enter)),
        (Action::OpenExternal, chord("gx")),
        // Panes.
        (Action::NextPane, n(Key::Tab)),
        (Action::PrevPane, n(Key::BackTab)),
        (Action::SyncOtherPaneHere, c('=')),
        // View.
        (Action::ToggleHidden, c('.')),
        (Action::ToggleSyncBrowse, c('S')),
        (Action::ToggleUnionView, c('u')),
        (Action::ToggleDirSizes, c('z')),
        // `o`/`O` for ordering rather than `s`, which is now purely the sync
        // prefix: leaving sort on `s` would make every sort keystroke wait for
        // the chord timeout before doing anything.
        (Action::CycleSort, c('o')),
        (Action::ReverseSort, c('O')),
        // Marks. `m` is a prefix rather than a toggle so the mark commands
        // read as a group; Space is the one-key toggle you actually use.
        (Action::ToggleMark, n(Key::Space)),
        (Action::MarkAll, chord("ma")),
        (Action::ClearMarks, chord("mc")),
        (Action::InvertMarks, chord("mi")),
        // Search.
        (Action::Search, c('/')),
        (Action::SearchNext, c('n')),
        (Action::SearchPrev, c('N')),
        // File operations. `n` is search-repeat here, so the "new" keys are
        // `a`/`A` rather than the bookmark list's `n` -- one letter apart in
        // meaning and nowhere near each other on the keyboard, which is the
        // point: `n` in a browser should never create anything.
        (Action::NewFile, c('a')),
        (Action::NewDir, c('A')),
        (Action::Rename, c('r')),
        (Action::Chmod, c('p')),
        // `e` for edit, matching the connection list's own `e`. The two act
        // on different things -- a file here, a saved host there -- but both
        // mean "open this for editing", which is the association worth
        // keeping across modes.
        (Action::EditFile, c('e')),
        // A chord for the destructive one, matching `[keys.bookmarks]`: a
        // single `d` next to `a` is how you delete the file you meant to
        // create beside it.
        (Action::DeleteFile, chord("dd")),
        // Sync. All chords under `s`, so the destructive one cannot be
        // reached by a single keystroke -- and `s` alone is the sort key, so
        // the prefix is already in use for something harmless.
        (Action::SyncPush, chord("sp")),
        (Action::SyncPull, chord("sl")),
        (Action::SyncMirror, chord("sm")),
        (Action::SyncProfile, chord("ss")),
        // `sb` for bisync. Under the same `s` prefix as the rest, because it
        // deletes on both sides once a snapshot exists and must not be one
        // keystroke away.
        (Action::SyncBisync, chord("sb")),
        // `sw` for watch, under the same prefix. A watcher left on by
        // accident syncs on every save, so it is not one keystroke away.
        (Action::ToggleWatch, chord("sw")),
        // The audit log. Under `s` with the rest of the sync commands,
        // because what it shows is which syncs ran.
        (Action::ShowLog, chord("sL")),
        // Back to the connection list, which Esc drops out of. `c` for
        // connections: free in this mode, and not under the `s` prefix
        // because this is navigation, not a sync command.
        (Action::ShowConnections, c('c')),
        // General.
        (Action::Help, c('?')),
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
        (Action::Quit, c('q')),
        (Action::Quit, chord("ZZ")),
    ]);
    Keymap(bindings)
}

fn confirm() -> Keymap {
    let mut bindings = motions();
    bindings.extend([
        // `y`/`n` rather than Enter/Esc alone: the question this window asks
        // is "shall I overwrite these", and a misplaced Enter should not be
        // the thing that answers it. Enter is bound too, but second.
        (Action::Apply, c('y')),
        (Action::Apply, n(Key::Enter)),
        (Action::Cancel, c('n')),
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
        (Action::Cancel, c('q')),
        // Per-row inclusion.
        (Action::ToggleRow, n(Key::Space)),
        (Action::InvertRows, c('i')),
        (Action::AllRows, c('a')),
        (Action::OnlyRow, c('o')),
        // Inspection before deciding.
        (Action::Checksum, c('c')),
        (Action::DiffRow, c('D')),
        (Action::EnterDir, c('l')),
        (Action::ParentDir, c('h')),
        // Folds, spelled as in vim.
        (Action::FoldSection, chord("zc")),
        (Action::UnfoldSection, chord("zo")),
        // Conflict resolution, on the row under the cursor. Capitals, so
        // they cannot be hit while reaching for a motion key -- settling a
        // conflict the wrong way is exactly as destructive as the thing this
        // window exists to prevent. `L` and `R` read as the two sides;
        // `B` keeps both; `X` defers.
        //
        // `L`/`R` are free in this mode: the window has no horizontal
        // movement, since `h`/`l` drill into and out of a directory's rows.
        (Action::ResolveKeepLocal, c('L')),
        (Action::ResolveKeepRemote, c('R')),
        (Action::ResolveKeepBoth, c('B')),
        (Action::ResolveSkip, c('X')),
        (Action::Help, c('?')),
    ]);
    Keymap(bindings)
}

/// The two-button yes/no dialog.
///
/// Its own map rather than the confirmation window's, because the dialog is
/// the one place where `y` and `n` are *answers* rather than commands, and a
/// layout that moves `n` onto something else -- Colemak puts a motion there --
/// must not take the answer with it. The answer keys are therefore bound
/// here, where nothing else competes for them.
///
/// The selection keys matter as much as the letters: the dialog starts on
/// "no", so Enter alone is always the safe answer, and the arrows move
/// between the two buttons for anybody whose fingers are not on `y`.
fn question() -> Keymap {
    Keymap(vec![
        // Move between the buttons. `h`/`l` as well as the arrows, because
        // this is a horizontal choice and those are the horizontal keys.
        (Action::PrevChoice, n(Key::Left)),
        (Action::PrevChoice, c('h')),
        (Action::NextChoice, n(Key::Right)),
        (Action::NextChoice, c('l')),
        (Action::NextChoice, n(Key::Tab)),
        // Enter answers with whatever is selected, which is "no" until you
        // move. Enter cannot mean "yes" here: the dialog guards closing a
        // connection and deleting a bookmark, and a stray Enter must not be
        // what confirms one.
        (Action::Submit, n(Key::Enter)),
        (Action::Submit, n(Key::Space)),
        // The letters answer directly, without moving first.
        (Action::Apply, c('y')),
        (Action::Cancel, c('n')),
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
        (Action::Cancel, c('q')),
    ])
}

fn bookmarks() -> Keymap {
    let mut bindings = motions();
    bindings.extend([
        // Enter and `l` both open, so the key that enters a directory in the
        // browser also opens a connection here.
        (Action::OpenItem, n(Key::Enter)),
        (Action::OpenItem, c('l')),
        (Action::NewItem, c('n')),
        (Action::EditItem, c('e')),
        // A chord for the destructive one, deliberately: a single `d` next to
        // `e` is how you delete the bookmark you meant to edit.
        (Action::DeleteItem, chord("dd")),
        (Action::Refresh, ctrl('r')),
        // `n` is new-connection here, following lazygit, so search-repeat has
        // no key in this mode -- the list is short enough that `/` landing on
        // the first match is all it needs.
        (Action::Search, c('/')),
        (Action::Help, c('?')),
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
        (Action::Quit, c('q')),
        (Action::Quit, chord("ZZ")),
    ]);
    Keymap(bindings)
}

fn form() -> Keymap {
    // Printable keys are text here, so this map stays deliberately thin: only
    // the keys that cannot be literal input are bound. See
    // `Mode::takes_text_input`.
    //
    // That is also why choice fields cycle with the arrow keys rather than
    // `h`/`l` or space: those have to stay available for typing a path or a
    // password that happens to contain them.
    Keymap(vec![
        (Action::MoveDown, n(Key::Down)),
        (Action::MoveDown, n(Key::Tab)),
        (Action::MoveUp, n(Key::Up)),
        (Action::MoveUp, n(Key::BackTab)),
        (Action::NextChoice, n(Key::Right)),
        (Action::PrevChoice, n(Key::Left)),
        (Action::Submit, n(Key::Enter)),
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
    ])
}

fn prompt() -> Keymap {
    Keymap(vec![
        (Action::Submit, n(Key::Enter)),
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
    ])
}

fn help() -> Keymap {
    let mut bindings = motions();
    bindings.extend([
        // `?` closes the help it opened, which is what pressing it again
        // should obviously do.
        (Action::Cancel, c('?')),
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
        (Action::Cancel, c('q')),
        (Action::Quit, chord("ZZ")),
    ]);
    Keymap(bindings)
}

fn queue() -> Keymap {
    let mut bindings = motions();
    bindings.extend([
        // `p` for pause, and Esc to cancel. Cancelling is Esc rather than `q`
        // because `q` in every other mode quits lazysync, and quitting
        // mid-transfer should not be one keystroke away from pausing it.
        (Action::TogglePause, c('p')),
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
        (Action::Help, c('?')),
    ]);
    Keymap(bindings)
}

fn log() -> Keymap {
    let mut bindings = motions();
    bindings.extend([
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, ctrl('c')),
        (Action::Cancel, c('q')),
        (Action::Search, c('/')),
        (Action::SearchNext, c('n')),
        (Action::SearchPrev, c('N')),
        (Action::Help, c('?')),
    ]);
    Keymap(bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{combo::Mods, keymap::Resolution};

    /// Esc must never be the only way out of a mode.
    ///
    /// A terminal multiplexer can take Esc before lazysync ever sees it --
    /// binding it to close a floating pane is a normal thing to do, and
    /// zellij and tmux both intercept at that level. When that happens, a
    /// mode whose only Cancel is Esc has no way out at all: on the bookmark
    /// list you could not reach the local browser, in the queue you could
    /// not cancel a running transfer.
    ///
    /// So every mode carries a second Cancel key. `ctrl+c` rather than
    /// something mnemonic, because it has to be free in all eight modes and
    /// it already meant cancel in the form and the prompt.
    #[test]
    fn esc_is_never_the_only_way_to_cancel() {
        for mode in Mode::ALL {
            let map = for_mode(mode);
            let escapes: Vec<_> = map
                .combos_for(Action::Cancel)
                .into_iter()
                .filter(|combo| **combo != n(Key::Esc))
                .collect();

            assert!(
                !escapes.is_empty(),
                "{mode} can only be cancelled with Esc, so a multiplexer that eats Esc \
                 leaves it with no way out"
            );
        }
    }

    /// A duplicated combo within one mode means the second binding is dead
    /// code the user cannot reach, which is a mistake in this file rather than
    /// something to resolve at runtime.
    #[test]
    fn no_mode_binds_the_same_combo_twice() {
        for mode in Mode::ALL {
            let map = for_mode(mode);
            for (i, (action, combo)) in map.iter().enumerate() {
                for (other_action, other) in map.iter().skip(i + 1) {
                    assert_ne!(
                        combo, other,
                        "{mode} binds {combo} to both {action} and {other_action}"
                    );
                }
            }
        }
    }

    /// A chord whose prefix is bound to something else in the same mode costs
    /// the user a timeout on every press of the shorter binding. Allowed, but
    /// it should be a deliberate choice rather than an accident, so the test
    /// pins the exceptions.
    #[test]
    fn chord_prefixes_do_not_shadow_single_keys_by_accident() {
        for mode in Mode::ALL {
            let map = for_mode(mode);
            for (action, combo) in map.iter() {
                if combo.len() < 2 {
                    continue;
                }
                let prefix = &combo.0[..1];
                if let Resolution::Pending {
                    fallback: Some(shadowed),
                } = map.resolve(prefix)
                {
                    panic!(
                        "{mode}: pressing {} alone fires {shadowed} only after the chord \
                         timeout, because {combo} ({action}) starts with it",
                        KeyCombo(prefix.to_vec()),
                    );
                }
            }
        }
    }

    #[test]
    fn vim_motions_are_present_in_the_browser() {
        let map = browser();
        assert_eq!(
            map.resolve(&[ModifiedKey::plain(Key::Char('j'))]),
            Resolution::Exact(Action::MoveDown)
        );
        assert_eq!(
            map.resolve(&[
                ModifiedKey::plain(Key::Char('g')),
                ModifiedKey::plain(Key::Char('g'))
            ]),
            Resolution::Exact(Action::MoveTop)
        );
        assert_eq!(
            map.resolve(&[ModifiedKey::new(Key::Char('d'), Mods::ctrl())]),
            Resolution::Exact(Action::HalfPageDown)
        );
    }

    /// Text-input modes must not bind bare printable keys, or typing a
    /// password would trip commands.
    #[test]
    fn text_input_modes_bind_no_bare_printable_keys() {
        for mode in Mode::ALL.into_iter().filter(|m| m.takes_text_input()) {
            for (action, combo) in for_mode(mode).iter() {
                for key in &combo.0 {
                    assert!(
                        !(key.mods == Mods::NONE && key.key.is_text()),
                        "{mode} binds printable {combo} to {action}, which would swallow typed text"
                    );
                }
            }
        }
    }
}
