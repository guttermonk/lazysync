//! Keys, modifiers and chords.
//!
//! Three layers, matching the three shapes a binding can take in config:
//!
//! ```toml
//! "S"                             # Key           -> ModifiedKey -> KeyCombo
//! { key = "d", mod = ["Ctrl"] }   # ModifiedKey   -> KeyCombo
//! ["g", "g"]                      # KeyCombo
//! "ctrl+d"                        # shorthand for the map form
//! ```

use std::{fmt, str::FromStr};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// A single key, with no modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    F(u8),
    Enter,
    Space,
    Tab,
    BackTab,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
}

impl Key {
    /// The spelling accepted in config and printed by `lazysync keys`.
    fn name(self) -> String {
        match self {
            Self::Char(c) => c.to_string(),
            Self::F(n) => format!("F{n}"),
            Self::Enter => "enter".into(),
            Self::Space => "space".into(),
            Self::Tab => "tab".into(),
            Self::BackTab => "backtab".into(),
            Self::Backspace => "backspace".into(),
            Self::Esc => "esc".into(),
            Self::Up => "up".into(),
            Self::Down => "down".into(),
            Self::Left => "left".into(),
            Self::Right => "right".into(),
            Self::Home => "home".into(),
            Self::End => "end".into(),
            Self::PageUp => "pageup".into(),
            Self::PageDown => "pagedown".into(),
            Self::Delete => "delete".into(),
            Self::Insert => "insert".into(),
        }
    }

    /// True for keys that insert text in a prompt or form.
    pub const fn is_text(self) -> bool {
        matches!(self, Self::Char(_) | Self::Space)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

impl FromStr for Key {
    type Err = ParseKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // A single character is itself, including "A" -- see the note on
        // ModifiedKey about why uppercase does not also carry Shift.
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Ok(Self::Char(c));
        }

        // Function keys are the one case-sensitive-ish name; accept f1 too.
        if let Some(rest) = s.strip_prefix(['F', 'f']) {
            if let Ok(n) = rest.parse::<u8>() {
                if (1..=24).contains(&n) {
                    return Ok(Self::F(n));
                }
                return Err(ParseKeyError::FunctionKeyRange(n));
            }
        }

        // Named keys, with the aliases people actually type.
        Ok(match s.to_ascii_lowercase().as_str() {
            "enter" | "return" | "cr" => Self::Enter,
            "space" => Self::Space,
            "tab" => Self::Tab,
            "backtab" | "shifttab" => Self::BackTab,
            "backspace" | "bs" => Self::Backspace,
            "esc" | "escape" => Self::Esc,
            "up" => Self::Up,
            "down" => Self::Down,
            "left" => Self::Left,
            "right" => Self::Right,
            "home" => Self::Home,
            "end" => Self::End,
            "pageup" | "pgup" => Self::PageUp,
            "pagedown" | "pgdn" => Self::PageDown,
            "delete" | "del" => Self::Delete,
            "insert" | "ins" => Self::Insert,
            _ => return Err(ParseKeyError::UnknownKey(s.to_owned())),
        })
    }
}

/// Which modifiers a binding requires.
///
/// A plain struct of flags rather than a bitflags dependency; there are four
/// of them and they are never iterated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        super_: false,
    };

    pub const fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Self::NONE
        }
    }

    pub const fn shift() -> Self {
        Self {
            shift: true,
            ..Self::NONE
        }
    }

    /// Set one flag by its config spelling. Accumulates, so a `mod` list can
    /// be folded in one name at a time.
    pub(super) fn set(&mut self, name: &str) -> Result<(), ParseKeyError> {
        match name.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "c" => self.ctrl = true,
            "alt" | "meta" | "a" | "m" => self.alt = true,
            "shift" | "s" => self.shift = true,
            "super" | "os" | "cmd" | "win" | "d" => self.super_ = true,
            _ => return Err(ParseKeyError::UnknownModifier(name.to_owned())),
        }
        Ok(())
    }

    fn from_crossterm(m: KeyModifiers) -> Self {
        Self {
            ctrl: m.contains(KeyModifiers::CONTROL),
            alt: m.contains(KeyModifiers::ALT),
            shift: m.contains(KeyModifiers::SHIFT),
            super_: m.contains(KeyModifiers::SUPER),
        }
    }
}

impl fmt::Display for Mods {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (flag, name) in [
            (self.ctrl, "ctrl"),
            (self.alt, "alt"),
            (self.shift, "shift"),
            (self.super_, "super"),
        ] {
            if flag {
                write!(f, "{name}+")?;
            }
        }
        Ok(())
    }
}

/// One key press: a key plus the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModifiedKey {
    pub key: Key,
    pub mods: Mods,
}

impl ModifiedKey {
    pub const fn new(key: Key, mods: Mods) -> Self {
        Self { key, mods }
    }

    pub const fn plain(key: Key) -> Self {
        Self::new(key, Mods::NONE)
    }

    pub const fn ctrl(c: char) -> Self {
        Self::new(Key::Char(c), Mods::ctrl())
    }

    /// Translate a terminal key event into a binding lookup key.
    ///
    /// Returns `None` for events that are not a key press -- key *releases*
    /// and *repeats* arrive on terminals that negotiate the kitty keyboard
    /// protocol, and acting on a release makes every binding fire twice.
    ///
    /// Two normalisations matter:
    ///
    /// - Shift is dropped when the key is already an uppercase character.
    ///   The terminal reports `Shift+a` as `Char('A') + SHIFT`, so leaving
    ///   Shift on would mean `"G"` in a config file never matches, and
    ///   `["Z", "Z"]` could not be written at all.
    /// - `BackTab` absorbs its Shift for the same reason: crossterm reports
    ///   Shift+Tab as `BackTab + SHIFT`.
    pub fn from_event(event: KeyEvent) -> Option<Self> {
        if !matches!(event.kind, KeyEventKind::Press) {
            return None;
        }

        let key = match event.code {
            KeyCode::Char(' ') => Key::Space,
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::F(n) => Key::F(n),
            KeyCode::Enter => Key::Enter,
            KeyCode::Tab => Key::Tab,
            KeyCode::BackTab => Key::BackTab,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Esc => Key::Esc,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Delete => Key::Delete,
            KeyCode::Insert => Key::Insert,
            _ => return None,
        };

        let mut mods = Mods::from_crossterm(event.modifiers);
        let shift_is_implied =
            matches!(key, Key::Char(c) if c.is_uppercase()) || matches!(key, Key::BackTab);
        if shift_is_implied {
            mods.shift = false;
        }

        Some(Self { key, mods })
    }
}

impl fmt::Display for ModifiedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.mods, self.key)
    }
}

impl FromStr for ModifiedKey {
    type Err = ParseKeyError;

    /// Parses `"d"`, `"ctrl+d"`, `"ctrl+alt+Delete"`, and `"+"`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(ParseKeyError::Empty);
        }

        // "+" and "ctrl++" both end in a literal plus, so the final segment is
        // taken by position rather than by splitting and hoping.
        let (mod_part, key_part) = match s.rfind('+') {
            // A trailing '+' means the key *is* '+': "ctrl++" or just "+".
            Some(i) if i + 1 == s.len() => (&s[..i], "+"),
            Some(i) => (&s[..i], &s[i + 1..]),
            None => ("", s),
        };

        let mut mods = Mods::NONE;
        for name in mod_part.split('+').filter(|p| !p.is_empty()) {
            mods.set(name)?;
        }

        let key = key_part.parse::<Key>()?;

        // "ctrl+A" and "ctrl+shift+a" are the same press; normalise to the
        // former so a config written either way matches the same events.
        if matches!(key, Key::Char(c) if c.is_uppercase()) {
            mods.shift = false;
        }

        Ok(Self { key, mods })
    }
}

/// A key sequence: one press, or a chord like `gg`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombo(pub Vec<ModifiedKey>);

impl KeyCombo {
    pub fn single(key: ModifiedKey) -> Self {
        Self(vec![key])
    }

    pub fn chord(keys: impl IntoIterator<Item = ModifiedKey>) -> Self {
        Self(keys.into_iter().collect())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// True when `prefix` is a leading subsequence of this combo.
    ///
    /// A combo is its own prefix, which is what makes an exact match and a
    /// longer chord both visible to the resolver at once.
    pub fn starts_with(&self, prefix: &[ModifiedKey]) -> bool {
        self.0.len() >= prefix.len() && self.0[..prefix.len()] == *prefix
    }
}

impl fmt::Display for KeyCombo {
    /// `gg`, `ctrl+d`, `m a`.
    ///
    /// Unmodified single characters run together the way vim writes them;
    /// anything else is space-separated, because `ctrl+dd` would be ambiguous.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let all_plain_chars = self
            .0
            .iter()
            .all(|k| k.mods == Mods::NONE && matches!(k.key, Key::Char(_)));
        let separator = if all_plain_chars { "" } else { " " };

        for (i, key) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(separator)?;
            }
            write!(f, "{key}")?;
        }
        Ok(())
    }
}

/// Why a key, modifier or combo in a config file could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseKeyError {
    #[error("empty key")]
    Empty,
    #[error(
        "unknown key {0:?} (expected a single character, F1-F24, or one of: enter, space, tab, \
         backtab, backspace, esc, up, down, left, right, home, end, pageup, pagedown, delete, \
         insert)"
    )]
    UnknownKey(String),
    #[error("unknown modifier {0:?} (expected Ctrl, Alt, Shift or Super)")]
    UnknownModifier(String),
    #[error("F{0} is out of range (expected F1-F24)")]
    FunctionKeyRange(u8),
}
