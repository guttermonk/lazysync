//! lazysync -- a dual-pane terminal file manager and sync tool.
//!
//! The crate is laid out along the phases in `docs/PLAN.md`. Phase 1 is the
//! browser, config and keymap engine over the local filesystem; the protocol
//! backends, the sync planner and the confirmation window follow.
//!
//! The one structural rule worth knowing before reading further: no widget
//! inspects a key event. [`event::Dispatcher`] resolves a press into a
//! [`keys::Action`] using the keymap for the current [`keys::Mode`], and
//! [`app::App`] decides what that action does. That is what lets the help
//! overlay be generated from the live keymap, and what keeps `d` a command in
//! the browser and a literal character in a password field.

pub mod app;
pub mod bookmarks;
pub mod browser;
pub mod cli;
pub mod config;
pub mod connection;
pub mod event;
pub mod integrations;
pub mod keys;
pub mod sync;
pub mod tui;
pub mod ui;
pub mod vfs;

/// The version reported by `lazysync --version` and in bug reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
