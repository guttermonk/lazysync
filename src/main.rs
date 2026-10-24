//! Binary entry point.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use lazysync::{
    app::App,
    cli::{self, Cli, Command},
    config::{self, Config},
    keys::Mode,
    tui,
};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        // The read-only subcommands log to stderr: there is no TUI to corrupt,
        // and a diagnostic command that hides its own warnings in a file is
        // useless.
        Some(Command::Keys { .. } | Command::Config) => {
            init_stderr_logging();
            run_query(&cli)
        }
        Some(Command::Open { name }) => {
            init_stderr_logging();
            not_yet("open", 2, &format!("bookmark {name:?}"))
        }
        Some(Command::Sync { profile, .. }) => {
            init_stderr_logging();
            not_yet("sync", 3, &format!("profile {profile:?}"))
        }
        Some(Command::Import { from }) => {
            init_stderr_logging();
            not_yet("import", 2, &format!("importing from {from:?}"))
        }
        None => run_tui(&cli),
    }
}

/// `lazysync keys` and `lazysync config`.
fn run_query(cli: &Cli) -> Result<()> {
    let config = Config::load_default(cli.config.as_deref())?;

    match &cli.command {
        Some(Command::Keys { mode }) => {
            let keymaps = config.keymaps();

            let only = match mode {
                Some(name) => Some(Mode::parse(name).with_context(|| {
                    let names: Vec<_> = Mode::ALL.iter().map(|m| m.name()).collect();
                    format!(
                        "unknown mode {name:?} (expected one of: {})",
                        names.join(", ")
                    )
                })?),
                None => None,
            };

            for (mode, keymap) in keymaps.iter() {
                if only.is_some_and(|wanted| wanted != mode) {
                    continue;
                }
                println!("[keys.{mode}]");
                for action in keymap.actions() {
                    let combos: Vec<_> = keymap
                        .combos_for(action)
                        .iter()
                        .map(ToString::to_string)
                        .collect();
                    println!("  {:<20} {}", action.name(), combos.join(", "));
                }
                println!();
            }
        }
        Some(Command::Config) => {
            // Emitted as TOML rather than as a debug dump so the output can be
            // pasted back into config.toml and mean the same thing.
            let rendered =
                toml::to_string_pretty(&config).context("rendering the config as TOML")?;
            println!("# {}", config::config_path().display());
            print!("{rendered}");
        }
        _ => unreachable!("run_query is only called for Keys and Config"),
    }

    Ok(())
}

/// The interactive browser.
fn run_tui(cli: &Cli) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let (left, right) = cli.pane_targets(&cwd);

    // Phase 2 turns a URL into a backend. Refusing now beats opening the
    // local filesystem and pretending the URL was honoured.
    for target in [&left, &right] {
        if cli::looks_like_url(target) {
            anyhow::bail!(
                "connecting to {target:?} needs the protocol backends, which arrive in \
                 phase 2 -- see docs/PLAN.md"
            );
        }
    }

    if !tui::is_interactive() {
        anyhow::bail!("lazysync needs a terminal; use `lazysync sync <profile>` for cron");
    }

    let config = Config::load_default(cli.config.as_deref())?;

    // Keep the guard alive for the whole run: dropping it stops the
    // background log writer, which would silently lose every later line.
    let _log_guard = tui::init_file_logging("lazysync=warn");

    let mut app = App::new(config, PathBuf::from(left), PathBuf::from(right))?;

    tui::run(&mut app)
}

fn init_stderr_logging() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter =
        EnvFilter::try_from_env("LAZYSYNC_LOG").unwrap_or_else(|_| EnvFilter::new("lazysync=warn"));

    // `try_init`, not `init`: a second call would panic, and these commands
    // are cheap enough that someone will eventually call one twice in a test.
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Stand-in for the subcommands whose phase has not landed.
///
/// Naming the phase rather than just failing means running the scaffolding
/// tells you what is missing instead of looking broken.
fn not_yet(what: &str, phase: u8, detail: &str) -> Result<()> {
    anyhow::bail!(
        "lazysync {what}: not implemented yet ({detail}). Arrives in phase {phase} \
         -- see docs/PLAN.md"
    )
}
