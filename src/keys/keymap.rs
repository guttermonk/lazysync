//! Keymaps, and how a key press becomes an action.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    action::{Action, Mode},
    combo::{Key, KeyCombo, ModifiedKey, Mods},
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

    /// Every binding that continues `prefix`, in binding order.
    ///
    /// What a chord in progress could still become: after `s`, this is
    /// `sp push`, `sl pull`, and the rest. The resolver already knows a
    /// longer combo exists -- that is what [`Resolution::Pending`] means --
    /// and this is the same question asked for the user's benefit rather
    /// than the dispatcher's.
    ///
    /// An empty prefix returns nothing. Every binding in the mode continues
    /// "no keys pressed", and listing all of them is what `?` is for.
    pub fn completions(&self, prefix: &[ModifiedKey]) -> Vec<(Action, &KeyCombo)> {
        if prefix.is_empty() {
            return Vec::new();
        }

        self.0
            .iter()
            .filter(|(_, combo)| combo.len() > prefix.len() && combo.starts_with(prefix))
            .map(|(action, combo)| (*action, combo))
            .collect()
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
///
/// Four tables, applied in the order they are listed here. They exist
/// because "change one key" and "change every key" are different jobs, and
/// so is "change which key does X" versus "change what X's key does":
///
/// - `base` — start from nothing but this.
/// - `set` — these actions get exactly these keys, and lose the ones they
///   had. The common case, and the one `extra` cannot express.
/// - `extra` — these keys get these actions, on top of whatever is there.
/// - `unbind` — these keys do nothing at all. Applied last, so it is a veto.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeymapSection {
    /// Replaces the built-in defaults for this mode entirely.
    pub base: Option<Keymap>,
    /// Rebinds whole actions: every listed action loses its existing keys and
    /// gets the listed ones instead. Bare arrow keys are kept -- see
    /// [`apply_set`].
    ///
    /// This is the difference from `extra`. `extra` resolves collisions by
    /// *key* -- it drops whatever else was on the key you took -- so
    /// rebinding `MoveDown` to `e` with `extra` leaves `j` moving down too.
    /// `set` resolves by *action*, which is what "I moved this to `e`"
    /// actually means.
    pub set: Option<Keymap>,
    /// Layered on top, winning any collision.
    pub extra: Option<Keymap>,
    /// Keys that must do nothing in this mode.
    ///
    /// For keys something else needs to see: a multiplexer that closes its
    /// popup on Esc only gets Esc if lazysync is not sitting on it.
    pub unbind: Option<Vec<KeyCombo>>,
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
    /// Per mode: `base` replaces the defaults if given, then `set`, then
    /// `extra`, then `unbind`.
    ///
    /// `unbind` is last on purpose. It is the only one of the four that
    /// subtracts, and a veto that another table could reinstate would be no
    /// veto at all -- the whole reason to reach for it is to guarantee a key
    /// reaches the terminal instead of lazysync.
    pub fn resolve(sections: &BTreeMap<Mode, KeymapSection>) -> Self {
        let mut maps = BTreeMap::new();

        for mode in Mode::ALL {
            let section = sections.get(&mode);
            let mut map = section
                .and_then(|s| s.base.clone())
                .unwrap_or_else(|| defaults::for_mode(mode));

            if let Some(set) = section.and_then(|s| s.set.as_ref()) {
                apply_set(&mut map, set);
            }
            if let Some(extra) = section.and_then(|s| s.extra.as_ref()) {
                apply_extra(&mut map, extra);
            }
            if let Some(unbind) = section.and_then(|s| s.unbind.as_ref()) {
                apply_unbind(&mut map, unbind);
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

/// A bare arrow key, which `set` does not retire.
///
/// Modified arrows are not included: `ctrl+down` is somebody's deliberate
/// binding rather than the key everyone reaches for, so it is replaced like
/// any other.
fn is_bare_arrow(combo: &KeyCombo) -> bool {
    matches!(
        combo.0.as_slice(),
        [ModifiedKey {
            key: Key::Up | Key::Down | Key::Left | Key::Right,
            mods: Mods::NONE,
        }]
    )
}

/// Move whole actions onto new keys.
///
/// Two things are dropped before the new bindings go on: every existing
/// binding **of a listed action**, which is the point, and every binding
/// whose combo the new keys make unreachable, which is the same prefix rule
/// `apply_extra` needs and for the same reason.
///
/// The order matters and is not interchangeable. Actions are cleared first,
/// so `set = [["MoveDown", "j"]]` -- rebinding an action to a key it already
/// had -- ends with one binding rather than zero.
///
/// **Arrow keys survive.** `set = [["MoveDown", "e"]]` means "move down is on
/// `e` now", and nobody means by that "and the Down arrow should stop
/// working". The arrows are not part of the layout being chosen -- they are
/// the keys that work when you have not learned the layout yet, including for
/// whoever else sits at the keyboard -- so a statement about `j` and `e` is
/// not a statement about them. Requiring every `set` to restate `down`,
/// `up`, `left` and `right` would mean the cost of forgetting is a key that
/// silently stops working, which is the failure this whole area keeps
/// producing.
///
/// Retiring an arrow is still possible and is now something you say rather
/// than something you cause: `unbind = ["down"]` runs after `set` and removes
/// it.
fn apply_set(map: &mut Keymap, set: &Keymap) {
    let rebound: Vec<Action> = set.iter().map(|(action, _)| *action).collect();
    map.0.retain(|(action, combo)| {
        let replaced = rebound.contains(action) && !is_bare_arrow(combo);
        if replaced {
            tracing::debug!("binding {combo} -> {action} dropped: the action was `set` elsewhere");
        }
        !replaced
    });

    for (_, new_combo) in set.iter() {
        map.0.retain(|(action, combo)| {
            let shadowed = combo.starts_with(&new_combo.0);
            if shadowed {
                tracing::debug!("binding {combo} -> {action} dropped in favour of {new_combo}");
            }
            !shadowed
        });
    }

    map.0.extend(set.0.iter().cloned());
}

/// Remove every binding a listed combo would reach.
///
/// Prefix-based, like the shadowing in `apply_extra`: `unbind = ["s"]` has to
/// take the `sp`/`sl`/`sm` chords with it, or pressing `s` would still be
/// swallowed as the start of a chord and the key would not actually be free.
/// Unbinding a key that is already unbound is not an error -- the state asked
/// for is the state you get.
fn apply_unbind(map: &mut Keymap, combos: &[KeyCombo]) {
    for dead in combos {
        map.0.retain(|(action, combo)| {
            let unreachable = combo.starts_with(&dead.0);
            if unreachable {
                tracing::debug!("binding {combo} -> {action} removed: {dead} is unbound");
            }
            !unreachable
        });
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

    fn key(c: char) -> ModifiedKey {
        ModifiedKey::plain(Key::Char(c))
    }

    fn combo(keys: &[char]) -> KeyCombo {
        KeyCombo(keys.iter().copied().map(key).collect())
    }

    fn map(bindings: &[(Action, &[char])]) -> Keymap {
        Keymap(
            bindings
                .iter()
                .map(|(action, keys)| (*action, combo(keys)))
                .collect(),
        )
    }

    /// What the which-key bar asks: having pressed `s`, what comes next?
    #[test]
    fn completions_list_what_a_chord_could_still_become() {
        let keymap = map(&[
            (Action::SyncPush, &['s', 'p']),
            (Action::SyncPull, &['s', 'l']),
            (Action::MoveDown, &['j']),
        ]);

        let offered: Vec<Action> = keymap
            .completions(&[key('s')])
            .into_iter()
            .map(|(action, _)| action)
            .collect();

        assert_eq!(
            offered,
            vec![Action::SyncPush, Action::SyncPull],
            "binding order, and nothing that does not continue `s`"
        );
    }

    /// Every binding continues "nothing pressed yet". Listing them all is
    /// what the help overlay is for, so an empty prefix offers nothing.
    #[test]
    fn an_empty_prefix_offers_nothing() {
        let keymap = map(&[(Action::SyncPush, &['s', 'p'])]);
        assert!(keymap.completions(&[]).is_empty());
    }

    /// A combo is its own prefix, and a binding is not a completion of
    /// itself -- otherwise `gg` would offer `gg`.
    #[test]
    fn a_complete_binding_is_not_a_completion_of_itself() {
        let keymap = map(&[(Action::MoveTop, &['g', 'g'])]);
        assert!(keymap.completions(&[key('g'), key('g')]).is_empty());
        assert_eq!(keymap.completions(&[key('g')]).len(), 1);
    }

    /// The real keymap, which is what the bar renders: `s` is the prefix the
    /// sync chords hide behind, and it is the one nobody guesses.
    #[test]
    fn the_default_browser_map_offers_the_sync_chords_under_s() {
        let keymap = crate::keys::defaults::for_mode(Mode::Browser);

        let offered: Vec<Action> = keymap
            .completions(&[key('s')])
            .into_iter()
            .map(|(action, _)| action)
            .collect();

        for expected in [Action::SyncPush, Action::SyncPull, Action::SyncMirror] {
            assert!(
                offered.contains(&expected),
                "{expected} missing: {offered:?}"
            );
        }
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

    /// The gap `set` exists to fill.
    ///
    /// `extra` resolves collisions by key, so it can only ever *add* a key to
    /// an action. Moving `MoveDown` to `e` with `extra` leaves `j` moving
    /// down as well, which is not what "I moved it" means to anybody.
    #[test]
    fn extra_cannot_retire_the_key_it_replaces_but_set_can() {
        let mut with_extra = map(&[(Action::MoveDown, &['j'])]);
        apply_extra(&mut with_extra, &map(&[(Action::MoveDown, &['e'])]));
        assert_eq!(
            with_extra.resolve(&[key('j')]),
            Resolution::Exact(Action::MoveDown),
            "this is the shortcoming: `j` still works"
        );

        let mut with_set = map(&[(Action::MoveDown, &['j'])]);
        apply_set(&mut with_set, &map(&[(Action::MoveDown, &['e'])]));
        assert_eq!(
            with_set.resolve(&[key('e')]),
            Resolution::Exact(Action::MoveDown)
        );
        assert_eq!(
            with_set.resolve(&[key('j')]),
            Resolution::None,
            "`set` retires the old key"
        );
    }

    /// `set` is a statement about the layout, not about the arrows.
    #[test]
    fn set_keeps_the_arrow_key_it_did_not_mention() {
        let mut m = map(&[(Action::MoveDown, &['j'])]);
        m.0.push((
            Action::MoveDown,
            KeyCombo::single(ModifiedKey::plain(Key::Down)),
        ));

        apply_set(&mut m, &map(&[(Action::MoveDown, &['e'])]));

        assert_eq!(m.resolve(&[key('e')]), Resolution::Exact(Action::MoveDown));
        assert_eq!(m.resolve(&[key('j')]), Resolution::None, "j was replaced");
        assert_eq!(
            m.resolve(&[ModifiedKey::plain(Key::Down)]),
            Resolution::Exact(Action::MoveDown),
            "the Down arrow is not part of choosing between j and e"
        );
    }

    /// All four arrows, and only bare ones.
    #[test]
    fn only_unmodified_arrows_are_spared() {
        for arrow in [Key::Up, Key::Down, Key::Left, Key::Right] {
            let mut m = Keymap(vec![(
                Action::MoveDown,
                KeyCombo::single(ModifiedKey::plain(arrow)),
            )]);
            apply_set(&mut m, &map(&[(Action::MoveDown, &['e'])]));
            assert_eq!(
                m.resolve(&[ModifiedKey::plain(arrow)]),
                Resolution::Exact(Action::MoveDown),
                "{arrow:?} should have survived"
            );
        }

        // A modified arrow is a deliberate binding, so it is replaced.
        let mut m = Keymap(vec![(
            Action::MoveDown,
            KeyCombo::single(ModifiedKey::new(Key::Down, Mods::ctrl())),
        )]);
        apply_set(&mut m, &map(&[(Action::MoveDown, &['e'])]));
        assert_eq!(
            m.resolve(&[ModifiedKey::new(Key::Down, Mods::ctrl())]),
            Resolution::None,
            "ctrl+down is somebody's own binding, not the universal arrow"
        );
    }

    /// Sparing the arrows must not resurrect one for an action that is being
    /// moved off it onto a different arrow.
    #[test]
    fn setting_an_action_onto_an_arrow_does_not_duplicate_it() {
        let mut m = Keymap(vec![(
            Action::MoveDown,
            KeyCombo::single(ModifiedKey::plain(Key::Down)),
        )]);
        let replacement = Keymap(vec![(
            Action::MoveDown,
            KeyCombo::single(ModifiedKey::plain(Key::Down)),
        )]);
        apply_set(&mut m, &replacement);

        assert_eq!(m.0.len(), 1, "one binding, not two: {:?}", m.0);
        assert_eq!(
            m.resolve(&[ModifiedKey::plain(Key::Down)]),
            Resolution::Exact(Action::MoveDown)
        );
    }

    /// Taking an arrow for a different action still displaces the old owner,
    /// or sparing arrows would mean two actions on one key.
    #[test]
    fn an_arrow_claimed_by_another_action_changes_hands() {
        let mut m = Keymap(vec![(
            Action::MoveDown,
            KeyCombo::single(ModifiedKey::plain(Key::Down)),
        )]);
        let replacement = Keymap(vec![(
            Action::MoveUp,
            KeyCombo::single(ModifiedKey::plain(Key::Down)),
        )]);
        apply_set(&mut m, &replacement);

        assert_eq!(
            m.resolve(&[ModifiedKey::plain(Key::Down)]),
            Resolution::Exact(Action::MoveUp),
            "an explicit claim on the arrow wins"
        );
        assert_eq!(m.0.len(), 1);
    }

    /// The escape hatch: `unbind` still retires an arrow, and runs after
    /// `set`, so the sparing rule is a default rather than a trap.
    #[test]
    fn unbind_can_still_retire_an_arrow_that_set_spared() {
        let mut base = map(&[(Action::MoveDown, &['j'])]);
        base.0.push((
            Action::MoveDown,
            KeyCombo::single(ModifiedKey::plain(Key::Down)),
        ));

        let sections = BTreeMap::from([(
            Mode::Browser,
            KeymapSection {
                base: Some(base),
                set: Some(map(&[(Action::MoveDown, &['e'])])),
                extra: None,
                unbind: Some(vec![KeyCombo::single(ModifiedKey::plain(Key::Down))]),
            },
        )]);

        let resolved = Keymaps::resolve(&sections);
        let m = resolved.get(Mode::Browser);

        assert_eq!(m.resolve(&[key('e')]), Resolution::Exact(Action::MoveDown));
        assert_eq!(
            m.resolve(&[ModifiedKey::plain(Key::Down)]),
            Resolution::None,
            "saying so explicitly still works"
        );
    }

    /// Only the listed actions are touched; everything else keeps its keys.
    /// That is the difference from `base`.
    #[test]
    fn set_leaves_every_other_action_alone() {
        let mut m = map(&[
            (Action::MoveDown, &['j']),
            (Action::MoveUp, &['k']),
            (Action::Quit, &['q']),
        ]);
        apply_set(&mut m, &map(&[(Action::MoveDown, &['e'])]));

        assert_eq!(m.resolve(&[key('k')]), Resolution::Exact(Action::MoveUp));
        assert_eq!(m.resolve(&[key('q')]), Resolution::Exact(Action::Quit));
    }

    /// Giving an action several keys at once replaces all of its old ones.
    #[test]
    fn set_can_give_one_action_several_keys() {
        let mut m = map(&[(Action::MoveDown, &['j'])]);
        let mut replacement = map(&[(Action::MoveDown, &['e'])]);
        replacement.0.push((Action::MoveDown, combo(&['n'])));
        apply_set(&mut m, &replacement);

        assert_eq!(m.resolve(&[key('e')]), Resolution::Exact(Action::MoveDown));
        assert_eq!(m.resolve(&[key('n')]), Resolution::Exact(Action::MoveDown));
        assert_eq!(m.resolve(&[key('j')]), Resolution::None);
    }

    /// Rebinding an action to a key it already had must not cancel itself
    /// out. This is why actions are cleared before the new ones are added.
    #[test]
    fn set_to_the_same_key_is_not_a_removal() {
        let mut m = map(&[(Action::MoveDown, &['j'])]);
        apply_set(&mut m, &map(&[(Action::MoveDown, &['j'])]));

        assert_eq!(m.resolve(&[key('j')]), Resolution::Exact(Action::MoveDown));
        assert_eq!(m.0.len(), 1, "and not duplicated either");
    }

    /// Taking a key that belonged to something else frees it, same as `extra`.
    #[test]
    fn set_displaces_whatever_held_the_new_key() {
        let mut m = map(&[(Action::MoveDown, &['j']), (Action::MoveUp, &['e'])]);
        apply_set(&mut m, &map(&[(Action::MoveDown, &['e'])]));

        assert_eq!(m.resolve(&[key('e')]), Resolution::Exact(Action::MoveDown));
        assert_eq!(
            m.resolve(&[key('j')]),
            Resolution::None,
            "MoveDown moved off j"
        );
        assert!(
            m.combos_for(Action::MoveUp).is_empty(),
            "MoveUp lost the key it was on, and is not silently somewhere else"
        );
    }

    /// `unbind` makes a key do nothing, which is the point when something
    /// outside lazysync needs to see it.
    #[test]
    fn unbind_frees_a_key_entirely() {
        let mut m = map(&[(Action::Cancel, &['\u{1b}']), (Action::Quit, &['q'])]);
        m.0[0].1 = KeyCombo::single(ModifiedKey::plain(Key::Esc));

        apply_unbind(&mut m, &[KeyCombo::single(ModifiedKey::plain(Key::Esc))]);

        assert_eq!(
            m.resolve(&[ModifiedKey::plain(Key::Esc)]),
            Resolution::None,
            "Esc must reach the terminal, not lazysync"
        );
        assert_eq!(m.resolve(&[key('q')]), Resolution::Exact(Action::Quit));
    }

    /// Unbinding a chord prefix takes the whole family, or the key would
    /// still be swallowed while lazysync waited for the second press.
    #[test]
    fn unbind_takes_the_chords_a_key_starts() {
        let mut m = map(&[(Action::SyncPush, &['s', 'p']), (Action::Quit, &['q'])]);
        apply_unbind(&mut m, &[combo(&['s'])]);

        assert_eq!(
            m.resolve(&[key('s')]),
            Resolution::None,
            "`s` must not sit pending on a chord that no longer exists"
        );
        assert_eq!(m.resolve(&[key('q')]), Resolution::Exact(Action::Quit));
    }

    #[test]
    fn unbinding_something_already_unbound_is_not_an_error() {
        let mut m = map(&[(Action::Quit, &['q'])]);
        apply_unbind(&mut m, &[combo(&['z'])]);
        assert_eq!(m.resolve(&[key('q')]), Resolution::Exact(Action::Quit));
    }

    /// `unbind` is applied after `extra`, so it cannot be reinstated by a
    /// table the user also wrote. A veto that loses is not a veto.
    #[test]
    fn unbind_wins_over_extra() {
        let sections = BTreeMap::from([(
            Mode::Browser,
            KeymapSection {
                base: Some(map(&[(Action::Quit, &['q'])])),
                set: None,
                extra: Some(map(&[(Action::Cancel, &['x'])])),
                unbind: Some(vec![combo(&['x'])]),
            },
        )]);

        let resolved = Keymaps::resolve(&sections);
        assert_eq!(
            resolved.get(Mode::Browser).resolve(&[key('x')]),
            Resolution::None
        );
    }

    /// All four tables in one section, in the documented order.
    #[test]
    fn the_four_tables_compose() {
        let sections = BTreeMap::from([(
            Mode::Browser,
            KeymapSection {
                base: Some(map(&[
                    (Action::MoveDown, &['j']),
                    (Action::MoveUp, &['k']),
                    (Action::Quit, &['q']),
                ])),
                set: Some(map(&[(Action::MoveDown, &['e'])])),
                extra: Some(map(&[(Action::Help, &['?'])])),
                unbind: Some(vec![combo(&['q'])]),
            },
        )]);

        let m = Keymaps::resolve(&sections);
        let m = m.get(Mode::Browser);

        assert_eq!(m.resolve(&[key('e')]), Resolution::Exact(Action::MoveDown));
        assert_eq!(m.resolve(&[key('j')]), Resolution::None, "set retired it");
        assert_eq!(m.resolve(&[key('k')]), Resolution::Exact(Action::MoveUp));
        assert_eq!(m.resolve(&[key('?')]), Resolution::Exact(Action::Help));
        assert_eq!(m.resolve(&[key('q')]), Resolution::None, "unbound");
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
                ..Default::default()
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
