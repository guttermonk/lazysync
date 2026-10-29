//! Footer key hints, built from the resolved keymap.
//!
//! **A hint names the key that is actually bound.** Every footer in lazysync
//! used to be a string literal listing the default keys, which is correct
//! exactly until somebody rebinds something -- and then the application is
//! confidently telling them to press a key that does nothing. That is worse
//! than no hint at all: an empty footer makes you open `?`, a wrong one makes
//! you doubt the program.
//!
//! It was reported on the bookmark list, whose footer said `n new` against a
//! keymap that bound `NewItem` to `a`. The same bug was in the confirmation
//! window, where the footer said `n cancel` while `n` was bound to
//! `ParentDir` -- a key that does not cancel, named as the way out of the one
//! screen in lazysync that is about to overwrite files.
//!
//! So hints are generated from the same `Keymap` the dispatcher resolves
//! against, which makes the two impossible to disagree. The help screen
//! already worked this way; this is the rest of the application catching up.
//!
//! **An unbound action is omitted, not shown bound to nothing.** If a user's
//! `base` map has no `Checksum`, the confirm footer simply does not offer it,
//! because a footer's entire job is to list keys that work.

use crate::keys::{Action, Keymap};

/// Render `items` as ` <key> <label> · <key> <label>`, skipping any action
/// the keymap does not bind.
///
/// The *first* bound combo wins, which is binding order, which is the order
/// the user wrote their config in -- so somebody who lists their preferred
/// key first sees that one in the footer rather than whichever sorts lowest.
pub fn line(keymap: &Keymap, items: &[(Action, &str)]) -> String {
    let rendered: Vec<String> = items
        .iter()
        .filter_map(|(action, label)| {
            let combo = keymap
                .combos_for(*action)
                .first()
                .map(ToString::to_string)?;
            Some(format!("{combo} {label}"))
        })
        .collect();

    if rendered.is_empty() {
        return String::new();
    }
    format!(" {}", rendered.join(" \u{b7} "))
}

/// Like [`line`], but drops trailing items that will not fit in `width`.
///
/// Whole items are dropped rather than the string being cut off, because half
/// a hint (`sp pu`) is not a shorter hint, it is a wrong one. Items are
/// therefore given most-useful-first by the caller, and the tail is what goes
/// when the terminal is narrow.
pub fn fitted(keymap: &Keymap, items: &[(Action, &str)], width: usize) -> String {
    let resolved: Vec<(String, &str)> = items
        .iter()
        .filter_map(|(action, label)| Some((key_for(keymap, *action)?, *label)))
        .collect();

    fitted_pairs(&resolved, width)
}

/// [`fitted`] for hints whose key is already known.
///
/// The which-key bar works the other way round from every other caller: it
/// starts from the combos that continue a chord and asks what each one does,
/// rather than starting from actions and asking what each is bound to.
pub fn fitted_pairs(items: &[(String, &str)], width: usize) -> String {
    let mut out = String::new();
    for (combo, label) in items {
        let piece = if out.is_empty() {
            format!(" {combo} {label}")
        } else {
            format!(" \u{b7} {combo} {label}")
        };
        if out.chars().count() + piece.chars().count() > width {
            break;
        }
        out.push_str(&piece);
    }
    out
}

/// The key bound to one action, for prose rather than a footer.
///
/// Returns `None` when nothing is bound, so the caller has to say something
/// that still makes sense without a key -- see the bookmark list's empty
/// state, which drops the sentence rather than naming a key that is not there.
pub fn key_for(keymap: &Keymap, action: Action) -> Option<String> {
    keymap.combos_for(action).first().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{defaults, KeyCombo, Mode};

    fn combo(spec: &str) -> KeyCombo {
        KeyCombo::chord(spec.split('+').map(|k| k.parse().unwrap()))
    }

    /// The bug this module exists for, as reported: the bookmark footer said
    /// `n new` against a keymap that bound `NewItem` to `a`.
    #[test]
    fn a_rebound_key_shows_up_in_the_hint() {
        let config = r#"
[keys.bookmarks]
base = [
    ["NewItem", "a"],
    ["EditItem", "c"],
]
"#;
        let parsed: crate::config::Config = toml::from_str(config).unwrap();
        let keymaps = parsed.keymaps();
        let keymap = keymaps.get(Mode::Bookmarks);

        let hint = line(
            keymap,
            &[(Action::NewItem, "new"), (Action::EditItem, "edit")],
        );
        assert_eq!(
            hint, " a new \u{b7} c edit",
            "the footer has to name the bound key, not the default"
        );
        assert!(
            !hint.contains("n new"),
            "`n new` is what the old literal said, and pressing `n` did nothing"
        );
    }

    /// A footer that lists a key doing nothing is the failure being fixed, so
    /// an action with no binding is left out entirely.
    #[test]
    fn an_unbound_action_is_omitted() {
        let keymap = Keymap(vec![(Action::NewItem, combo("a"))]);

        let hint = line(
            &keymap,
            &[(Action::NewItem, "new"), (Action::DeleteItem, "delete")],
        );
        assert_eq!(hint, " a new", "delete is unbound, so it is not offered");
    }

    #[test]
    fn every_item_unbound_gives_an_empty_line_rather_than_a_stray_separator() {
        let keymap = Keymap(Vec::new());
        assert_eq!(line(&keymap, &[(Action::NewItem, "new")]), "");
    }

    /// A chord is one hint, not two keys.
    #[test]
    fn a_chord_renders_as_the_sequence_it_is() {
        let keymap = defaults::for_mode(Mode::Bookmarks);
        assert_eq!(key_for(&keymap, Action::DeleteItem).as_deref(), Some("dd"));
    }

    /// Binding order is the user's order, so their first choice leads.
    #[test]
    fn the_first_bound_combo_wins() {
        let keymap = Keymap(vec![
            (Action::OpenItem, combo("o")),
            (Action::OpenItem, combo("enter")),
        ]);
        assert_eq!(key_for(&keymap, Action::OpenItem).as_deref(), Some("o"));
    }
}
