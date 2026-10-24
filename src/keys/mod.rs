//! Keybindings: what the keys are, what they do, and how config overrides
//! them.
//!
//! The shape is borrowed from inlyne -- an action enum paired with a key
//! combo, `base` replacing the defaults and `extra` layering over them -- with
//! one addition it does not need: a separate keymap per [`Mode`], because `d`
//! is a command in the browser and a literal `d` in a password field.
//!
//! Nothing in the UI matches on a raw key event. Widgets receive an
//! [`Action`], which is why the help overlay can be generated from the live
//! keymap and cannot describe a binding that no longer exists.

pub mod action;
pub mod combo;
pub mod defaults;
pub mod keymap;
mod serialization;

pub use action::{Action, Mode};
pub use combo::{Key, KeyCombo, ModifiedKey, Mods, ParseKeyError};
pub use keymap::{Keymap, KeymapSection, Keymaps, Resolution};
