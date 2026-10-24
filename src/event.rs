//! Turning key presses into actions.
//!
//! Chords make this more than a lookup. After `g`, lazysync does not yet know
//! whether the user means `gg`, `g/`, or nothing at all, so the pressed keys
//! accumulate until they resolve -- or until the chord timeout decides for
//! them.
//!
//! The logic lives in [`Dispatcher`], apart from the terminal, so it can be
//! tested by feeding it keys instead of by driving a pty.

use std::time::{Duration, Instant};

use crate::keys::{Action, Keymaps, Mode, ModifiedKey, Resolution};

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
    /// `None` means block indefinitely: with no chord in progress there is
    /// nothing to time out, and waking up to do nothing would just spin.
    pub fn poll_timeout(&self) -> Option<Duration> {
        let started = self.started?;
        Some(self.timeout.saturating_sub(started.elapsed()))
    }

    /// Feed a key press.
    pub fn key(&mut self, mode: Mode, keymaps: &Keymaps, key: ModifiedKey) -> Dispatch {
        let keymap = keymaps.get(mode);

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
        let (mut d, maps) = dispatcher();
        assert!(d.poll_timeout().is_none());

        d.key(Mode::Browser, &maps, key('g'));
        assert!(d.poll_timeout().is_some());
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
            extra: None,
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
