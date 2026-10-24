//! Colours.
//!
//! `preset = "terminal"` is the default and uses [`Color::Reset`] plus the
//! sixteen ANSI colours, so lazysync inherits whatever palette the terminal
//! already has. The named presets are for when you want lazysync to look the
//! same in every terminal regardless.
//!
//! Any individual colour can be overridden on top of a preset:
//!
//! ```toml
//! [theme]
//! preset = "gruvbox-dark"
//!
//! [theme.colors]
//! border_focused = "#fabd2f"
//! danger = "red"
//! ```

use std::collections::BTreeMap;

use ratatui::style::Color;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `[theme]`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Theme {
    pub preset: Preset,
    /// Per-slot overrides applied over the preset.
    pub colors: BTreeMap<String, ColorSpec>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            preset: Preset::Terminal,
            colors: BTreeMap::new(),
        }
    }
}

impl Theme {
    /// The resolved palette: the preset, with `[theme.colors]` applied.
    ///
    /// Returns an error naming the unknown slot rather than ignoring it, for
    /// the same reason the config denies unknown fields.
    pub fn palette(&self) -> Result<Palette, UnknownColorSlot> {
        let mut palette = self.preset.palette();
        for (slot, spec) in &self.colors {
            palette.set(slot, spec.0)?;
        }
        Ok(palette)
    }
}

/// A built-in palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// Follow the terminal's own colours.
    Terminal,
    CatppuccinMocha,
    GruvboxDark,
    Nord,
}

/// Every colour the UI uses.
///
/// A flat struct rather than a map so that a missing slot is a compile error
/// instead of a black-on-black surprise at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub fg: Color,
    pub bg: Color,
    /// Secondary text: sizes, timestamps, the inactive pane's path.
    pub dim: Color,

    pub border: Color,
    pub border_focused: Color,

    pub dir: Color,
    pub file: Color,
    pub symlink: Color,
    pub executable: Color,
    /// Entries that exist on only one side in union view, and ghosted panes.
    pub ghost: Color,
    pub marked: Color,

    pub cursor_fg: Color,
    pub cursor_bg: Color,

    pub status_fg: Color,
    pub status_bg: Color,

    pub accent: Color,
    pub ok: Color,
    pub warn: Color,
    pub danger: Color,
}

impl Palette {
    fn set(&mut self, slot: &str, color: Color) -> Result<(), UnknownColorSlot> {
        let target = match slot {
            "fg" => &mut self.fg,
            "bg" => &mut self.bg,
            "dim" => &mut self.dim,
            "border" => &mut self.border,
            "border_focused" => &mut self.border_focused,
            "dir" => &mut self.dir,
            "file" => &mut self.file,
            "symlink" => &mut self.symlink,
            "executable" => &mut self.executable,
            "ghost" => &mut self.ghost,
            "marked" => &mut self.marked,
            "cursor_fg" => &mut self.cursor_fg,
            "cursor_bg" => &mut self.cursor_bg,
            "status_fg" => &mut self.status_fg,
            "status_bg" => &mut self.status_bg,
            "accent" => &mut self.accent,
            "ok" => &mut self.ok,
            "warn" => &mut self.warn,
            "danger" => &mut self.danger,
            other => return Err(UnknownColorSlot(other.to_owned())),
        };
        *target = color;
        Ok(())
    }
}

/// A `[theme.colors]` key that is not a real colour slot.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "unknown theme colour {0:?} (expected one of: fg, bg, dim, border, border_focused, dir, \
     file, symlink, executable, ghost, marked, cursor_fg, cursor_bg, status_fg, status_bg, \
     accent, ok, warn, danger)"
)]
pub struct UnknownColorSlot(pub String);

impl Preset {
    fn palette(self) -> Palette {
        match self {
            Self::Terminal => Palette {
                // Reset, not a concrete colour: it is what lets a transparent
                // or themed terminal background show through.
                fg: Color::Reset,
                bg: Color::Reset,
                dim: Color::DarkGray,
                border: Color::DarkGray,
                border_focused: Color::Cyan,
                dir: Color::Blue,
                file: Color::Reset,
                symlink: Color::Magenta,
                executable: Color::Green,
                ghost: Color::DarkGray,
                marked: Color::Yellow,
                cursor_fg: Color::Black,
                cursor_bg: Color::Cyan,
                status_fg: Color::Reset,
                status_bg: Color::Reset,
                accent: Color::Cyan,
                ok: Color::Green,
                warn: Color::Yellow,
                danger: Color::Red,
            },
            Self::CatppuccinMocha => Palette {
                fg: rgb(0xcdd6f4),
                bg: rgb(0x1e1e2e),
                dim: rgb(0x6c7086),
                border: rgb(0x45475a),
                border_focused: rgb(0x89b4fa),
                dir: rgb(0x89b4fa),
                file: rgb(0xcdd6f4),
                symlink: rgb(0xcba6f7),
                executable: rgb(0xa6e3a1),
                ghost: rgb(0x585b70),
                marked: rgb(0xf9e2af),
                cursor_fg: rgb(0x1e1e2e),
                cursor_bg: rgb(0x89b4fa),
                status_fg: rgb(0xcdd6f4),
                status_bg: rgb(0x181825),
                accent: rgb(0x89b4fa),
                ok: rgb(0xa6e3a1),
                warn: rgb(0xf9e2af),
                danger: rgb(0xf38ba8),
            },
            Self::GruvboxDark => Palette {
                fg: rgb(0xebdbb2),
                bg: rgb(0x282828),
                dim: rgb(0x928374),
                border: rgb(0x504945),
                border_focused: rgb(0xfabd2f),
                dir: rgb(0x83a598),
                file: rgb(0xebdbb2),
                symlink: rgb(0xd3869b),
                executable: rgb(0xb8bb26),
                ghost: rgb(0x665c54),
                marked: rgb(0xfabd2f),
                cursor_fg: rgb(0x282828),
                cursor_bg: rgb(0xfabd2f),
                status_fg: rgb(0xebdbb2),
                status_bg: rgb(0x3c3836),
                accent: rgb(0xfabd2f),
                ok: rgb(0xb8bb26),
                warn: rgb(0xfe8019),
                danger: rgb(0xfb4934),
            },
            Self::Nord => Palette {
                fg: rgb(0xd8dee9),
                bg: rgb(0x2e3440),
                dim: rgb(0x616e88),
                border: rgb(0x434c5e),
                border_focused: rgb(0x88c0d0),
                dir: rgb(0x81a1c1),
                file: rgb(0xd8dee9),
                symlink: rgb(0xb48ead),
                executable: rgb(0xa3be8c),
                ghost: rgb(0x4c566a),
                marked: rgb(0xebcb8b),
                cursor_fg: rgb(0x2e3440),
                cursor_bg: rgb(0x88c0d0),
                status_fg: rgb(0xd8dee9),
                status_bg: rgb(0x3b4252),
                accent: rgb(0x88c0d0),
                ok: rgb(0xa3be8c),
                warn: rgb(0xebcb8b),
                danger: rgb(0xbf616a),
            },
        }
    }
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

/// A colour in config: `"#89b4fa"`, `"0x89b4fa"`, or an ANSI name like
/// `"red"` / `"bright-blue"` / `"reset"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorSpec(pub Color);

impl<'de> Deserialize<'de> for ColorSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        parse_color(&raw)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl Serialize for ColorSpec {
    /// Hex for true colour, the ANSI name otherwise -- so a dumped config
    /// round-trips through [`parse_color`].
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let text = match self.0 {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            Color::Reset => "reset".into(),
            Color::Black => "black".into(),
            Color::Red => "red".into(),
            Color::Green => "green".into(),
            Color::Yellow => "yellow".into(),
            Color::Blue => "blue".into(),
            Color::Magenta => "magenta".into(),
            Color::Cyan => "cyan".into(),
            Color::Gray => "gray".into(),
            Color::DarkGray => "dark-gray".into(),
            Color::LightRed => "bright-red".into(),
            Color::LightGreen => "bright-green".into(),
            Color::LightYellow => "bright-yellow".into(),
            Color::LightBlue => "bright-blue".into(),
            Color::LightMagenta => "bright-magenta".into(),
            Color::LightCyan => "bright-cyan".into(),
            Color::White => "bright-white".into(),
            // Indexed colours have no spelling in the config syntax; emitting
            // the number keeps the dump honest rather than inventing a name.
            Color::Indexed(n) => format!("indexed-{n}"),
        };
        serializer.serialize_str(&text)
    }
}

fn parse_color(raw: &str) -> Result<Color, String> {
    let hex = raw
        .strip_prefix('#')
        .or_else(|| raw.strip_prefix("0x"))
        .or_else(|| raw.strip_prefix("0X"));

    if let Some(hex) = hex {
        if hex.len() != 6 {
            return Err(format!(
                "colour {raw:?} should have six hex digits, e.g. \"#89b4fa\""
            ));
        }
        let value =
            u32::from_str_radix(hex, 16).map_err(|_| format!("colour {raw:?} is not valid hex"))?;
        return Ok(rgb(value));
    }

    Ok(match raw.to_ascii_lowercase().replace('_', "-").as_str() {
        "reset" | "default" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" | "white" => Color::Gray,
        "dark-gray" | "dark-grey" => Color::DarkGray,
        "bright-red" => Color::LightRed,
        "bright-green" => Color::LightGreen,
        "bright-yellow" => Color::LightYellow,
        "bright-blue" => Color::LightBlue,
        "bright-magenta" => Color::LightMagenta,
        "bright-cyan" => Color::LightCyan,
        "bright-white" => Color::White,
        other => {
            return Err(format!(
                "unknown colour {other:?} (expected a hex value like \"#89b4fa\", or an ANSI \
                 name: reset, black, red, green, yellow, blue, magenta, cyan, gray, dark-gray, \
                 bright-red, bright-green, bright-yellow, bright-blue, bright-magenta, \
                 bright-cyan, bright-white)"
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_preset_leaves_the_background_alone() {
        let palette = Theme::default().palette().unwrap();
        assert_eq!(
            palette.bg,
            Color::Reset,
            "the default theme must not paint over a themed or transparent terminal"
        );
    }

    #[test]
    fn overrides_apply_on_top_of_a_preset() {
        // r##"..."## because the hex colours contain `"#`, which would end an
        // r#"..."# literal early.
        let theme: Theme = toml::from_str(
            r##"
            preset = "nord"
            [colors]
            border_focused = "#fabd2f"
            danger = "bright-red"
            "##,
        )
        .unwrap();

        let palette = theme.palette().unwrap();
        assert_eq!(palette.border_focused, Color::Rgb(0xfa, 0xbd, 0x2f));
        assert_eq!(palette.danger, Color::LightRed);
        // Untouched slots keep the preset's value.
        assert_eq!(palette.dir, Color::Rgb(0x81, 0xa1, 0xc1));
    }

    #[test]
    fn an_unknown_colour_slot_is_reported() {
        let theme: Theme = toml::from_str(
            r#"
            [colors]
            boarder = "red"
            "#,
        )
        .unwrap();
        let err = theme.palette().unwrap_err();
        assert!(err.to_string().contains("boarder"), "{err}");
    }

    #[test]
    fn hex_accepts_both_prefixes() {
        assert_eq!(parse_color("#1e1e2e").unwrap(), Color::Rgb(30, 30, 46));
        assert_eq!(parse_color("0x1e1e2e").unwrap(), Color::Rgb(30, 30, 46));
    }

    #[test]
    fn a_short_hex_value_says_so() {
        let err = parse_color("#abc").unwrap_err();
        assert!(err.contains("six hex digits"), "{err}");
    }
}
