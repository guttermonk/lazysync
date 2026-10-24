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
        // Sync. All chords under `s`, so the destructive one cannot be
        // reached by a single keystroke -- and `s` alone is the sort key, so
        // the prefix is already in use for something harmless.
        (Action::SyncPush, chord("sp")),
        (Action::SyncPull, chord("sl")),
        (Action::SyncMirror, chord("sm")),
        // General.
        (Action::Help, c('?')),
        (Action::Cancel, n(Key::Esc)),
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
        (Action::Help, c('?')),
    ]);
    Keymap(bindings)
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
        (Action::Cancel, c('q')),
        (Action::Quit, chord("ZZ")),
    ]);
    Keymap(bindings)
}

fn queue() -> Keymap {
    let mut bindings = motions();
    bindings.extend([
        (Action::Cancel, n(Key::Esc)),
        (Action::Cancel, c('q')),
        (Action::Help, c('?')),
    ]);
    Keymap(bindings)
}

fn log() -> Keymap {
    let mut bindings = motions();
    bindings.extend([
        (Action::Cancel, n(Key::Esc)),
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
