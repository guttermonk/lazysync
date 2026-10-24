//! Keymaps, and how a key press becomes an action.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    action::{Action, Mode},
    combo::{KeyCombo, ModifiedKey},
    defaults,
};

/// An ordered list of bindings for one mode.
///
/// Order matters: the first binding that matches wins, so a user's `extra`
/// (appended last) is found before a default it did not displace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Keymap(pub Vec<(Action, KeyCombo)>);

/// What a prefix of pressed keys means so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// No binding starts with these keys. Drop them.
    None,
    /// At least one longer chord starts with these keys, so wait for more.
    ///
    /// `fallback` is set when the keys pressed are *also* a complete binding
    /// on their own -- `d` bound alongside `dd`. The event loop fires the
    /// fallback if the chord timeout expires, which is what vim does.
    Pending { fallback: Option<Action> },
    /// These keys are a complete binding and nothing longer starts with them.
    Exact(Action),
}

impl Keymap {
    pub fn iter(&self) -> std::slice::Iter<'_, (Action, KeyCombo)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Decide what the keys pressed so far mean.
    pub fn resolve(&self, pressed: &[ModifiedKey]) -> Resolution {
        if pressed.is_empty() {
            return Resolution::None;
        }

        let mut exact = None;
        let mut has_longer = false;

        for (action, combo) in &self.0 {
            if !combo.starts_with(pressed) {
                continue;
            }
            if combo.len() == pressed.len() {
                // First match wins, so later duplicates of the same combo are
                // shadowed rather than fighting.
                if exact.is_none() {
                    exact = Some(*action);
                }
            } else {
                has_longer = true;
            }
        }

        match (exact, has_longer) {
            (Some(action), false) => Resolution::Exact(action),
            (fallback, true) => Resolution::Pending { fallback },
            (None, false) => Resolution::None,
        }
    }

    /// Every combo bound to `action`, for the help overlay.
    pub fn combos_for(&self, action: Action) -> Vec<&KeyCombo> {
        self.0
            .iter()
            .filter(|(a, _)| *a == action)
            .map(|(_, combo)| combo)
            .collect()
    }

    /// The actions in this map, in the order they are first bound.
    pub fn actions(&self) -> Vec<Action> {
        let mut seen = Vec::new();
        for (action, _) in &self.0 {
            if !seen.contains(action) {
                seen.push(*action);
            }
        }
        seen
    }
}

/// One mode's `[keys.<mode>]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeymapSection {
    /// Replaces the built-in defaults for this mode entirely.
    pub base: Option<Keymap>,
    /// Layered on top of `base` (or the defaults), winning any collision.
    pub extra: Option<Keymap>,
}

/// The resolved keymap for every mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymaps(BTreeMap<Mode, Keymap>);

impl Default for Keymaps {
    fn default() -> Self {
        Self(
            Mode::ALL
                .into_iter()
                .map(|mode| (mode, defaults::for_mode(mode)))
                .collect(),
        )
    }
}

impl Keymaps {
    /// Merge user config over the defaults.
    ///
    /// Per mode: `base` replaces the defaults if given, then each `extra`
    /// binding is applied on top.
    pub fn resolve(sections: &BTreeMap<Mode, KeymapSection>) -> Self {
        let mut maps = BTreeMap::new();

        for mode in Mode::ALL {
            let section = sections.get(&mode);
            let mut map = section
                .and_then(|s| s.base.clone())
                .unwrap_or_else(|| defaults::for_mode(mode));

            if let Some(extra) = section.and_then(|s| s.extra.as_ref()) {
                apply_extra(&mut map, extra);
            }

            maps.insert(mode, map);
        }

        Self(maps)
    }

    pub fn get(&self, mode: Mode) -> &Keymap {
        // Every mode is populated by both constructors, so this cannot miss.
        self.0
            .get(&mode)
            .expect("Keymaps is built with an entry for every Mode")
    }

    pub fn iter(&self) -> impl Iterator<Item = (Mode, &Keymap)> {
        self.0.iter().map(|(mode, map)| (*mode, map))
    }
}

/// Layer `extra` over `base`, dropping any base binding that `extra` makes
/// unreachable.
///
/// The prefix test is the subtle part. Binding `d` in `extra` has to remove a
/// base `dd`, not just a base `d`: with `dd` still present, pressing `d`
/// resolves to `Pending` and the new binding could only ever fire after the
/// chord timeout. Dropping every base combo that *starts with* an extra combo
/// is what makes an override take effect immediately.
fn apply_extra(base: &mut Keymap, extra: &Keymap) {
    for (_, extra_combo) in extra.iter() {
        base.0.retain(|(action, combo)| {
            let shadowed = combo.starts_with(&extra_combo.0);
            if shadowed {
                tracing::debug!(
                    "default binding {combo} -> {action} dropped in favour of {extra_combo}"
                );
            }
            !shadowed
        });
    }

    base.0.extend(extra.0.iter().cloned());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::combo::{Key, Mods};

    fn key(c: char) -> ModifiedKey {
        ModifiedKey::plain(Key::Char(c))
    }

    fn map(bindings: &[(Action, &[char])]) -> Keymap {
        Keymap(
            bindings
                .iter()
                .map(|(action, keys)| (*action, KeyCombo(keys.iter().copied().map(key).collect())))
                .collect(),
        )
    }

    #[test]
    fn single_key_resolves_exactly() {
        let m = map(&[(Action::MoveDown, &['j'])]);
        assert_eq!(m.resolve(&[key('j')]), Resolution::Exact(Action::MoveDown));
    }

    #[test]
    fn unbound_key_resolves_to_none() {
        let m = map(&[(Action::MoveDown, &['j'])]);
        assert_eq!(m.resolve(&[key('q')]), Resolution::None);
    }

    #[test]
    fn chord_waits_for_its_second_key() {
        let m = map(&[(Action::MoveTop, &['g', 'g'])]);
        assert_eq!(
            m.resolve(&[key('g')]),
            Resolution::Pending { fallback: None }
        );
        assert_eq!(
            m.resolve(&[key('g'), key('g')]),
            Resolution::Exact(Action::MoveTop)
        );
    }

    #[test]
    fn abandoned_chord_resolves_to_none() {
        let m = map(&[(Action::MoveTop, &['g', 'g'])]);
        assert_eq!(m.resolve(&[key('g'), key('x')]), Resolution::None);
    }

    /// `d` bound alongside `dd` is ambiguous until the timeout fires, so the
    /// resolver has to report both facts at once.
    #[test]
    fn prefix_that_is_also_a_binding_reports_a_fallback() {
        let m = map(&[(Action::Cancel, &['d']), (Action::DiffRow, &['d', 'd'])]);
        assert_eq!(
            m.resolve(&[key('d')]),
            Resolution::Pending {
                fallback: Some(Action::Cancel)
            }
        );
        assert_eq!(
            m.resolve(&[key('d'), key('d')]),
            Resolution::Exact(Action::DiffRow)
        );
    }

    #[test]
    fn extra_adds_to_the_defaults() {
        let mut base = map(&[(Action::MoveDown, &['j'])]);
        apply_extra(&mut base, &map(&[(Action::Quit, &['q'])]));

        assert_eq!(
            base.resolve(&[key('j')]),
            Resolution::Exact(Action::MoveDown)
        );
        assert_eq!(base.resolve(&[key('q')]), Resolution::Exact(Action::Quit));
    }

    #[test]
    fn extra_overrides_a_colliding_default() {
        let mut base = map(&[(Action::MoveDown, &['j'])]);
        apply_extra(&mut base, &map(&[(Action::Quit, &['j'])]));

        assert_eq!(base.resolve(&[key('j')]), Resolution::Exact(Action::Quit));
        assert_eq!(base.0.len(), 1, "the shadowed default should be gone");
    }

    /// The reason `apply_extra` tests prefixes rather than equality: rebinding
    /// `g` has to displace the default `gg`, or `g` could only fire after the
    /// chord timeout.
    #[test]
    fn extra_overrides_a_default_chord_it_prefixes() {
        let mut base = map(&[(Action::MoveTop, &['g', 'g'])]);
        apply_extra(&mut base, &map(&[(Action::GoHome, &['g'])]));

        assert_eq!(base.resolve(&[key('g')]), Resolution::Exact(Action::GoHome));
    }

    /// The converse is deliberately *not* symmetric: binding a longer chord
    /// leaves the shorter default in place, and the timeout decides. Losing
    /// `g` entirely because the user added `gq` would be surprising.
    #[test]
    fn extra_chord_leaves_a_shorter_default_as_a_fallback() {
        let mut base = map(&[(Action::GoHome, &['g'])]);
        apply_extra(&mut base, &map(&[(Action::Quit, &['g', 'q'])]));

        assert_eq!(
            base.resolve(&[key('g')]),
            Resolution::Pending {
                fallback: Some(Action::GoHome)
            }
        );
        assert_eq!(
            base.resolve(&[key('g'), key('q')]),
            Resolution::Exact(Action::Quit)
        );
    }

    #[test]
    fn base_replaces_the_defaults_entirely() {
        let sections = BTreeMap::from([(
            Mode::Browser,
            KeymapSection {
                base: Some(map(&[(Action::Quit, &['x'])])),
                extra: None,
            },
        )]);

        let maps = Keymaps::resolve(&sections);
        let browser = maps.get(Mode::Browser);

        assert_eq!(
            browser.resolve(&[key('x')]),
            Resolution::Exact(Action::Quit)
        );
        assert_eq!(
            browser.resolve(&[key('j')]),
            Resolution::None,
            "a `base` table should not inherit the default j binding"
        );
        // Other modes keep their defaults.
        assert!(!maps.get(Mode::Help).is_empty());
    }

    #[test]
    fn defaults_fill_every_mode() {
        let maps = Keymaps::default();
        for mode in Mode::ALL {
            assert!(
                !maps.get(mode).is_empty(),
                "{mode} has no default bindings; an unreachable mode is a bug, \
                 not a configuration choice"
            );
        }
    }

    /// Every default mode must offer a way out, or a user who opens it is
    /// stuck with SIGKILL.
    #[test]
    fn every_default_mode_can_be_left() {
        let maps = Keymaps::default();
        for mode in Mode::ALL {
            let map = maps.get(mode);
            let escapable = map.actions().iter().any(|a| {
                matches!(
                    a,
                    Action::Cancel | Action::Quit | Action::Submit | Action::Apply
                )
            });
            assert!(
                escapable,
                "{mode} has no Cancel, Quit, Submit or Apply binding"
            );
        }
    }

    #[test]
    fn uppercase_key_does_not_require_a_separate_shift() {
        let shift_g: ModifiedKey = "G".parse().unwrap();
        assert_eq!(shift_g.mods, Mods::NONE);
        assert_eq!(shift_g.key, Key::Char('G'));
    }
}
