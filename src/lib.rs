//! lazysync -- a dual-pane terminal file manager and sync tool.
//!
//! The crate is laid out along the phases in `docs/PLAN.md`; this is the
//! scaffolding commit, so the modules below are the ones that exist today and
//! the rest arrive with the phase that needs them.

pub mod cli;

/// The version reported by `lazysync --version` and in bug reports.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
