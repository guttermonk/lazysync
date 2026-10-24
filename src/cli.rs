//! Command line surface.
//!
//! Two entry points share one binary: the interactive TUI (no subcommand, or
//! `open`), and a headless planner/applier (`sync`) that a cron job or systemd
//! timer can call with no terminal attached. Keeping them in one binary means
//! the headless path uses the same planner the confirmation window renders, so
//! a `--dry-run` report and an interactive plan can never drift apart.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "lazysync",
    version,
    about = "Dual-pane terminal file manager and sync tool",
    long_about = None,
)]
pub struct Cli {
    /// Connection URL to open directly, e.g. `sftp://user@host/srv/www`.
    pub url: Option<String>,

    /// Use this config file instead of `$XDG_CONFIG_HOME/lazysync/config.toml`.
    #[arg(long, value_name = "FILE", global = true)]
    pub config: Option<PathBuf>,

    /// Match the working directory against bookmark local paths and open the
    /// match, overriding `[zide] auto_open_bookmark`.
    #[arg(long, conflicts_with = "no_auto_open")]
    pub auto_open: bool,

    /// Never auto-open a bookmark from the working directory.
    #[arg(long, conflicts_with = "auto_open")]
    pub no_auto_open: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open a saved bookmark by name.
    Open {
        /// Bookmark name as shown in the bookmark list.
        name: String,
    },

    /// Plan a sync, and apply it without a TUI. Intended for cron.
    Sync {
        /// Sync profile to run.
        profile: String,

        /// Print the plan and exit without transferring anything.
        #[arg(long)]
        dry_run: bool,

        /// Apply the plan without asking for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },

    /// Print the resolved keymap, after config `base`/`extra` merging.
    Keys {
        /// Limit output to one mode: browser, confirm, form, prompt, help,
        /// queue, log.
        #[arg(long)]
        mode: Option<String>,
    },

    /// Print the resolved configuration, with defaults filled in.
    Config,

    /// Import bookmarks from another tool.
    Import {
        /// Source application. Currently: `termscp`.
        from: String,
    },
}
