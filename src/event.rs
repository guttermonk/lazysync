//! Turning key presses into actions.
//!
//! Chords make this more than a lookup. After `g`, lazysync does not yet know
//! whether the user means `gg`, `g/`, or nothing at all, so the pressed keys
//! accumulate until they resolve -- or until the user backs out of them with
//! the mode's cancel key.
//!
//! A chord does not expire with the stock keymap. See
//! [`Dispatcher::poll_timeout`] for the one case that still needs a clock.
//!
//! The logic lives in [`Dispatcher`], apart from the terminal, so it can be
//! tested by feeding it keys instead of by driving a pty.

use std::time::{Duration, Instant};

use crate::keys::{Action, Keymap, Keymaps, Mode, ModifiedKey, Resolution};

/// What a key press amounted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    /// Run this action.
    Act(Action),
    /// A printable character in a text-input mode.
    Text(char),
    /// Backspace in a text-input mode.
    Backspace,
    /// Part of a chord: wait for the next key or the timeout.
    Pending,
    /// Bound to nothing, and not text. Nothing happens.
    Unbound,
}

/// Whether this key, on its own, is the mode's cancel key.
///
/// A single press rather than any combo bound to [`Action::Cancel`]: a chord
/// whose *first* key happened to be a cancel binding would otherwise abort
/// itself before it could be typed.
fn cancels(keymap: &Keymap, key: ModifiedKey) -> bool {
    keymap
        .combos_for(Action::Cancel)
        .iter()
        .any(|combo| combo.0.as_slice() == [key])
}

/// Accumulates key presses until they mean something.
pub struct Dispatcher {
    pending: Vec<ModifiedKey>,
    /// Action to run if the chord times out -- set when the keys pressed so
    /// far are a complete binding *and* the prefix of a longer one.
    fallback: Option<Action>,
    /// When the current partial chord started.
    started: Option<Instant>,
    timeout: Duration,
}

impl Dispatcher {
    pub fn new(timeout: Duration) -> Self {
        Self {
            pending: Vec::new(),
            fallback: None,
            started: None,
            timeout,
        }
    }

    /// How long to wait for the next key before the chord times out.
    ///
    /// `None` means block indefinitely, and that is the usual answer.
    ///
    /// **A chord only times out when there is something to fire.** The
    /// timeout exists for one ambiguity: `d` bound alongside `dd`, where
    /// lazysync has to decide how long to wait before concluding you meant
    /// the short one. With no such fallback there is nothing to decide, so
    /// the timeout can only ever *discard* a chord you are halfway through
    /// typing -- which is exactly what it did. Pressing `s`, reading the bar
    /// that had just appeared to tell you what `s` could become, and then
    /// pressing `p` got you nothing, because half a second had passed.
    ///
    /// None of lazysync's default chords has a fallback -- `defaults.rs`
    /// has a test that none shadows a single key -- so with a stock keymap
    /// this timeout never fired anything at all. It only threw chords away.
    ///
    /// So a chord now waits as long as you do. The way out of one you did
    /// not mean to start is to cancel it, which is a key you press rather
    /// than a wait you sit through.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.fallback?;

        let started = self.started?;
        Some(self.timeout.saturating_sub(started.elapsed()))
    }

    /// Feed a key press.
    pub fn key(&mut self, mode: Mode, keymaps: &Keymaps, key: ModifiedKey) -> Dispatch {
        let keymap = keymaps.get(mode);

        // Cancel applies to the innermost thing first, and a chord in
        // progress is inner to the screen behind it. So the key that closes
        // a dialog is also the key that means "never mind" halfway through
        // `sp` -- and because it is looked up rather than hardcoded, that is
        // Esc for most people and `ctrl+c` for anybody whose multiplexer
        // eats Esc.
        //
        // Without this there was no way out of a half-typed chord at all.
        // Waiting out the timeout is not a way out, and pressing some other
        // key is worse than nothing: the dead-end branch below deliberately
        // reconsiders it on its own, so backing out of `s` with `j` moves
        // the cursor down.
        if !self.pending.is_empty() && cancels(keymap, key) {
            self.clear();
            // Nothing ran. Cancel was spent on the chord, which is the point:
            // backing out of `sp` must not also close the screen you are on.
            return Dispatch::Unbound;
        }

        self.pending.push(key);

        match keymap.resolve(&self.pending) {
            Resolution::Exact(action) => {
                self.clear();
                Dispatch::Act(action)
            }
            Resolution::Pending { fallback } => {
                self.fallback = fallback;
                self.started = Some(Instant::now());
                Dispatch::Pending
            }
            Resolution::None => {
                // A dead end. If keys had accumulated, the new key is not
                // necessarily junk -- it may begin a binding of its own, the
                // way `g` then `j` should scroll down rather than do nothing.
                // So abandon the chord and reconsider this key alone.
                let restart = self.pending.len() > 1;
                self.clear();

                if restart {
                    return self.key(mode, keymaps, key);
                }

                self.as_text(mode, key)
            }
        }
    }

    /// Called when [`Self::poll_timeout`] elapses.
    ///
    /// Returns the fallback action if the abandoned chord was also a complete
    /// binding: `d` bound alongside `dd` fires `d` here, which is what vim
    /// does.
    pub fn timed_out(&mut self) -> Option<Action> {
        let action = self.fallback;
        self.clear();
        action
    }

    /// Whether a chord is in progress, for the status line.
    pub fn partial(&self) -> &[ModifiedKey] {
        &self.pending
    }

    fn clear(&mut self) {
        self.pending.clear();
        self.fallback = None;
        self.started = None;
    }

    /// An unbound key in a prompt or form is text to insert rather than an
    /// unknown command. Anywhere else it does nothing.
    fn as_text(&self, mode: Mode, key: ModifiedKey) -> Dispatch {
        use crate::keys::{Key, Mods};

        if !mode.takes_text_input() || key.mods != Mods::NONE {
            return Dispatch::Unbound;
        }

        match key.key {
            Key::Char(c) => Dispatch::Text(c),
            Key::Space => Dispatch::Text(' '),
            Key::Backspace => Dispatch::Backspace,
            _ => Dispatch::Unbound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{Key, Mods};

    fn key(c: char) -> ModifiedKey {
        ModifiedKey::plain(Key::Char(c))
    }

    fn dispatcher() -> (Dispatcher, Keymaps) {
        (
            Dispatcher::new(Duration::from_millis(500)),
            Keymaps::default(),
        )
    }

    #[test]
    fn a_single_bound_key_acts_immediately() {
        let (mut d, maps) = dispatcher();
        assert_eq!(
            d.key(Mode::Browser, &maps, key('j')),
            Dispatch::Act(Action::MoveDown)
        );
        assert!(d.partial().is_empty());
    }

    #[test]
    fn a_chord_waits_then_acts() {
        let (mut d, maps) = dispatcher();
        assert_eq!(d.key(Mode::Browser, &maps, key('g')), Dispatch::Pending);
        assert_eq!(d.partial().len(), 1);
        assert_eq!(
            d.key(Mode::Browser, &maps, key('g')),
            Dispatch::Act(Action::MoveTop)
        );
        assert!(d.partial().is_empty());
    }

    /// `g` then `j` should scroll down, not be swallowed because `gj` is
    /// unbound. Abandoning a chord has to reconsider the key that broke it.
    #[test]
    fn a_broken_chord_reconsiders_the_key_that_broke_it() {
        let (mut d, maps) = dispatcher();
        assert_eq!(d.key(Mode::Browser, &maps, key('g')), Dispatch::Pending);
        assert_eq!(
            d.key(Mode::Browser, &maps, key('j')),
            Dispatch::Act(Action::MoveDown)
        );
    }

    #[test]
    fn a_broken_chord_that_leads_nowhere_is_unbound() {
        let (mut d, maps) = dispatcher();
        d.key(Mode::Browser, &maps, key('g'));
        assert_eq!(d.key(Mode::Browser, &maps, key('\u{1}')), Dispatch::Unbound);
        assert!(d.partial().is_empty());
    }

    #[test]
    fn an_unbound_key_in_the_browser_does_nothing() {
        let (mut d, maps) = dispatcher();
        assert_eq!(d.key(Mode::Browser, &maps, key('\u{7}')), Dispatch::Unbound);
    }

    /// There is no timeout to wait for when no chord is in progress; blocking
    /// forever is correct and avoids a spin.
    #[test]
    fn no_chord_means_no_poll_timeout() {
        let (d, _) = dispatcher();
        assert!(d.poll_timeout().is_none());
    }

    /// The reported bug: pressing `s`, reading the bar that just appeared to
    /// say what `s` could become, and then pressing `p` did nothing, because
    /// the chord had been thrown away while it was being read.
    ///
    /// With the stock keymap there is no fallback to fire, so the timeout had
    /// nothing to decide and could only discard. A chord now waits.
    #[test]
    fn a_chord_with_nothing_to_fall_back_on_never_expires() {
        let (mut d, maps) = dispatcher();

        assert_eq!(d.key(Mode::Browser, &maps, key('s')), Dispatch::Pending);
        assert!(
            d.poll_timeout().is_none(),
            "nothing would fire, so there is nothing to wait for"
        );

        // And the second key still completes it, however long the pause was.
        assert_eq!(
            d.key(Mode::Browser, &maps, key('p')),
            Dispatch::Act(Action::SyncPush)
        );
    }

    /// The one case that still needs a clock: `d` bound alongside `dd` has a
    /// genuine ambiguity, and waiting is how vim resolves it.
    #[test]
    fn a_chord_with_a_fallback_still_expires() {
        use crate::keys::{KeyCombo, Keymap, KeymapSection};
        use std::collections::BTreeMap;

        let section = KeymapSection {
            base: Some(Keymap(vec![
                (Action::Cancel, KeyCombo::single(key('d'))),
                (Action::DiffRow, KeyCombo::chord([key('d'), key('d')])),
            ])),
            ..Default::default()
        };
        let maps = Keymaps::resolve(&BTreeMap::from([(Mode::Browser, section)]));

        let mut d = Dispatcher::new(Duration::from_millis(500));
        assert_eq!(d.key(Mode::Browser, &maps, key('d')), Dispatch::Pending);
        assert!(
            d.poll_timeout().is_some(),
            "`d` alone is a binding too, so how long to wait is a real question"
        );
    }

    /// Every default keymap, so the stock experience is the one above: no
    /// chord anywhere expires on its own.
    #[test]
    fn no_default_chord_expires_while_it_is_being_typed() {
        use crate::keys::{Key, Keymap};

        let maps = Keymaps::default();

        for mode in Mode::ALL {
            let keymap: &Keymap = maps.get(mode);
            for (_, combo) in keymap.iter().filter(|(_, c)| c.len() > 1) {
                let first = combo.0[0];
                // Skip a prefix that is also the mode's cancel key: pressing
                // it twice backs out, which has its own test.
                if first == ModifiedKey::plain(Key::Esc) {
                    continue;
                }

                let mut d = Dispatcher::new(Duration::from_millis(500));
                if d.key(mode, &maps, first) == Dispatch::Pending {
                    assert!(
                        d.poll_timeout().is_none(),
                        "{mode}: {combo} starts a chord that expires mid-typing"
                    );
                }
            }
        }
    }

    #[test]
    fn a_timed_out_chord_with_no_fallback_does_nothing() {
        let (mut d, maps) = dispatcher();
        d.key(Mode::Browser, &maps, key('g'));
        assert_eq!(d.timed_out(), None);
        assert!(d.partial().is_empty());
    }

    /// With `d` and `dd` both bound, pressing `d` alone has to fire `d` once
    /// the timeout expires rather than being lost.
    #[test]
    fn a_timed_out_chord_fires_its_fallback() {
        use crate::keys::{KeyCombo, Keymap, KeymapSection};
        use std::collections::BTreeMap;

        let section = KeymapSection {
            base: Some(Keymap(vec![
                (Action::Cancel, KeyCombo::single(key('d'))),
                (Action::DiffRow, KeyCombo::chord([key('d'), key('d')])),
            ])),
            ..Default::default()
        };
        let maps = Keymaps::resolve(&BTreeMap::from([(Mode::Browser, section)]));

        let mut d = Dispatcher::new(Duration::from_millis(1));
        assert_eq!(d.key(Mode::Browser, &maps, key('d')), Dispatch::Pending);
        assert_eq!(d.timed_out(), Some(Action::Cancel));
        assert!(d.partial().is_empty());
    }

    #[test]
    fn an_unbound_printable_key_is_text_in_a_prompt() {
        let (mut d, maps) = dispatcher();
        assert_eq!(d.key(Mode::Prompt, &maps, key('q')), Dispatch::Text('q'));
        assert_eq!(
            d.key(Mode::Prompt, &maps, ModifiedKey::plain(Key::Space)),
            Dispatch::Text(' ')
        );
        assert_eq!(
            d.key(Mode::Prompt, &maps, ModifiedKey::plain(Key::Backspace)),
            Dispatch::Backspace
        );
    }

    /// A prompt's own bindings still win over text: Enter submits, it does
    /// not type a newline.
    #[test]
    fn prompt_bindings_beat_text_input() {
        let (mut d, maps) = dispatcher();
        assert_eq!(
            d.key(Mode::Prompt, &maps, ModifiedKey::plain(Key::Enter)),
            Dispatch::Act(Action::Submit)
        );
        assert_eq!(
            d.key(Mode::Prompt, &maps, ModifiedKey::plain(Key::Esc)),
            Dispatch::Act(Action::Cancel)
        );
    }

    /// Backing out of a half-typed chord. Before this there was no way out
    /// of one: the timeout was a wait, not an answer, and any other key
    /// either continued the chord or got reconsidered on its own.
    #[test]
    fn the_cancel_key_backs_out_of_a_half_typed_chord() {
        let (mut d, maps) = dispatcher();

        assert_eq!(d.key(Mode::Browser, &maps, key('s')), Dispatch::Pending);
        assert_eq!(
            d.key(Mode::Browser, &maps, ModifiedKey::plain(Key::Esc)),
            Dispatch::Unbound,
            "the chord is abandoned and nothing else runs"
        );
        assert!(d.partial().is_empty());
        assert!(
            d.poll_timeout().is_none(),
            "no chord is in progress, so there is nothing left to time out"
        );
    }

    /// Cancel is spent on the chord, not on the screen behind it. Backing out
    /// of `sp` must not also close the pane you were in.
    #[test]
    fn cancelling_a_chord_does_not_cancel_the_screen() {
        let (mut d, maps) = dispatcher();

        // With nothing pending, the same key is an ordinary Cancel.
        assert_eq!(
            d.key(Mode::Confirm, &maps, ModifiedKey::plain(Key::Esc)),
            Dispatch::Act(Action::Cancel)
        );

        d.key(Mode::Confirm, &maps, key('z'));
        assert_eq!(
            d.key(Mode::Confirm, &maps, ModifiedKey::plain(Key::Esc)),
            Dispatch::Unbound,
            "with `z` pending, Esc is the chord's way out rather than the window's"
        );
    }

    /// The reason this is looked up rather than hardcoded: a multiplexer that
    /// closes its popup on Esc is a normal thing to configure, and `unbind =
    /// ["esc"]` is how the README says to give it up. That user's cancel key
    /// is `ctrl+c`, and it has to back out of a chord too.
    #[test]
    fn a_keymap_without_esc_backs_out_with_its_own_cancel_key() {
        let config: crate::config::Config = toml::from_str(
            r#"
[keys.browser]
unbind = ["esc"]
"#,
        )
        .unwrap();
        let maps = config.keymaps();
        let mut d = Dispatcher::new(Duration::from_millis(500));

        assert_eq!(d.key(Mode::Browser, &maps, key('s')), Dispatch::Pending);
        assert_eq!(
            d.key(Mode::Browser, &maps, ModifiedKey::plain(Key::Esc)),
            Dispatch::Unbound,
            "esc is unbound here, so it is a dead end -- but it must not run anything either"
        );

        assert_eq!(d.key(Mode::Browser, &maps, key('s')), Dispatch::Pending);
        assert_eq!(
            d.key(
                Mode::Browser,
                &maps,
                ModifiedKey::new(Key::Char('c'), Mods::ctrl())
            ),
            Dispatch::Unbound
        );
        assert!(
            d.partial().is_empty(),
            "ctrl+c is this keymap's cancel, so it is also its way out of a chord"
        );
    }

    /// A first key that is itself the cancel key still starts its chord,
    /// rather than aborting a chord that has not begun.
    #[test]
    fn the_cancel_key_still_starts_a_chord_of_its_own() {
        use crate::keys::{KeyCombo, Keymap, KeymapSection};
        use std::collections::BTreeMap;

        let section = KeymapSection {
            base: Some(Keymap(vec![
                (Action::Cancel, KeyCombo::single(key('q'))),
                (Action::MoveTop, KeyCombo::chord([key('q'), key('q')])),
            ])),
            ..Default::default()
        };
        let maps = Keymaps::resolve(&BTreeMap::from([(Mode::Browser, section)]));
        let mut d = Dispatcher::new(Duration::from_millis(500));

        assert_eq!(d.key(Mode::Browser, &maps, key('q')), Dispatch::Pending);
        assert_eq!(
            d.key(Mode::Browser, &maps, key('q')),
            Dispatch::Unbound,
            "the second press backs out of the chord, which is the price of \
             binding cancel to a chord prefix"
        );
    }

    /// Ctrl+something unbound must not be typed into a prompt as a character.
    #[test]
    fn a_modified_key_is_never_text() {
        let (mut d, maps) = dispatcher();
        assert_eq!(
            d.key(
                Mode::Prompt,
                &maps,
                ModifiedKey::new(Key::Char('x'), Mods::ctrl())
            ),
            Dispatch::Unbound
        );
    }

    /// Each mode uses its own keymap: `q` quits the browser and is a literal
    /// character in a prompt.
    #[test]
    fn the_same_key_differs_by_mode() {
        let (mut d, maps) = dispatcher();
        assert_eq!(
            d.key(Mode::Browser, &maps, key('q')),
            Dispatch::Act(Action::Quit)
        );
        assert_eq!(d.key(Mode::Prompt, &maps, key('q')), Dispatch::Text('q'));
    }
}
