//! Binary entry point.
//!
//! Phase 0 is scaffolding: the CLI surface is real and parses, and every
//! subcommand reports what it will do once the phase that implements it lands.
//! See `docs/PLAN.md`.

use anyhow::Result;
use clap::Parser;
use lazysync::cli::{Cli, Command};

fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    match cli.command {
        Some(Command::Open { name }) => unimplemented_phase("open", 2, &format!("bookmark {name}")),
        Some(Command::Sync { profile, .. }) => {
            unimplemented_phase("sync", 3, &format!("profile {profile}"))
        }
        Some(Command::Keys { .. }) => unimplemented_phase("keys", 1, "keymap dump"),
        Some(Command::Config) => unimplemented_phase("config", 1, "config dump"),
        Some(Command::Import { from }) => {
            unimplemented_phase("import", 2, &format!("import from {from}"))
        }
        None => unimplemented_phase("tui", 1, "interactive browser"),
    }
}

/// Stand-in for the not-yet-written subcommands.
///
/// It names the phase from `docs/PLAN.md` rather than just failing, so running
/// the scaffolding tells you what is missing instead of looking broken.
fn unimplemented_phase(what: &str, phase: u8, detail: &str) -> Result<()> {
    anyhow::bail!(
        "lazysync {}: not implemented yet ({detail}). Arrives in phase {phase} -- see docs/PLAN.md",
        what
    )
}

/// Logs to stderr, filtered by `LAZYSYNC_LOG` (e.g. `lazysync::sync=debug`).
///
/// Once the TUI owns the terminal this moves to a file under
/// `$XDG_STATE_HOME/lazysync/`, because anything written to stderr while
/// ratatui holds the alternate screen corrupts the display.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter =
        EnvFilter::try_from_env("LAZYSYNC_LOG").unwrap_or_else(|_| EnvFilter::new("lazysync=warn"));

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
