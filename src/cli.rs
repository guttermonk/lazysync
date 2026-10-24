//! Command line surface.
//!
//! Two entry points share one binary: the interactive TUI (no subcommand, or
//! `open`), and a headless planner/applier (`sync`) that a cron job or systemd
//! timer can call with no terminal attached. Keeping them in one binary means
//! the headless path uses the same planner the confirmation window renders, so
//! a `--dry-run` report and an interactive plan can never drift apart.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "lazysync",
    version,
    about = "Dual-pane terminal file manager and sync tool",
    long_about = None,
)]
pub struct Cli {
    /// What to open in the two panes.
    ///
    /// Nothing opens the working directory on both sides; one argument opens
    /// the working directory against it; two open one in each pane. A
    /// connection URL (`sftp://user@host/srv/www`) is accepted in either
    /// position.
    #[arg(value_name = "PATH_OR_URL", num_args = 0..=2)]
    pub targets: Vec<String>,

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

impl Cli {
    /// The two panes' starting points, filling in the working directory for
    /// whatever was not given.
    pub fn pane_targets(&self, cwd: &Path) -> (String, String) {
        let here = cwd.display().to_string();
        match self.targets.as_slice() {
            [] => (here.clone(), here),
            // One argument reads as "compare here against that", which is the
            // common case: `lazysync ~/backup/proj` from inside a project.
            [other] => (here, other.clone()),
            [left, right, ..] => (left.clone(), right.clone()),
        }
    }
}

/// Whether a target names a remote rather than a path.
///
/// Deliberately strict about the shape: a Windows drive letter (`C:\`) and a
/// relative path containing a colon must not be mistaken for a scheme.
pub fn looks_like_url(target: &str) -> bool {
    match target.split_once("://") {
        Some((scheme, _)) => {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+')
        }
        None => false,
    }
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("lazysync").chain(args.iter().copied()))
    }

    #[test]
    fn no_arguments_opens_the_working_directory_on_both_sides() {
        let cli = parse(&[]);
        let (left, right) = cli.pane_targets(Path::new("/home/greg/proj"));
        assert_eq!(left, "/home/greg/proj");
        assert_eq!(right, "/home/greg/proj");
    }

    /// One argument reads as "compare here against that", which is how you
    /// would use it from inside a project.
    #[test]
    fn one_argument_compares_the_working_directory_against_it() {
        let cli = parse(&["/mnt/backup/proj"]);
        let (left, right) = cli.pane_targets(Path::new("/home/greg/proj"));
        assert_eq!(left, "/home/greg/proj");
        assert_eq!(right, "/mnt/backup/proj");
    }

    #[test]
    fn two_arguments_fill_both_panes() {
        let cli = parse(&["/a", "/b"]);
        let (left, right) = cli.pane_targets(Path::new("/cwd"));
        assert_eq!((left.as_str(), right.as_str()), ("/a", "/b"));
    }

    #[test]
    fn urls_are_recognised() {
        assert!(looks_like_url("sftp://user@host/srv/www"));
        assert!(looks_like_url("s3://bucket/prefix"));
        assert!(looks_like_url("ftps://host"));
    }

    /// A path with a colon in it is still a path. Getting this wrong would
    /// send `C:\Users` or `./weird:name` down the connection code path.
    #[test]
    fn paths_containing_a_colon_are_not_urls() {
        assert!(!looks_like_url("/home/greg/proj"));
        assert!(!looks_like_url("./notes:draft"));
        assert!(!looks_like_url("C:\\Users\\greg"));
        assert!(!looks_like_url("relative/path"));
        assert!(!looks_like_url("://no-scheme"));
    }

    #[test]
    fn the_subcommands_parse() {
        assert!(matches!(
            parse(&["sync", "deploy", "--dry-run"]).command,
            Some(Command::Sync {
                dry_run: true,
                yes: false,
                ..
            })
        ));
        assert!(matches!(
            parse(&["keys", "--mode", "browser"]).command,
            Some(Command::Keys { mode: Some(_) })
        ));
        assert!(matches!(
            parse(&["open", "www"]).command,
            Some(Command::Open { .. })
        ));
    }

    /// The two auto-open flags contradict each other, so asking for both must
    /// be rejected rather than silently resolved one way.
    #[test]
    fn the_auto_open_flags_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["lazysync", "--auto-open", "--no-auto-open"]).is_err());
    }
}
