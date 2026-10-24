//! Binary entry point.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use lazysync::{
    app::App,
    cli::{self, Cli, Command},
    config::{self, Config},
    connection,
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
        Some(Command::Sync { profile, .. }) => {
            init_stderr_logging();
            not_yet("sync", 3, &format!("profile {profile:?}"))
        }
        Some(Command::Import { from }) => {
            init_stderr_logging();
            // Deferred from phase 2 deliberately: termscp's bookmark format
            // includes an encrypted password blob, and writing an importer
            // against a format that cannot be tested here would be guesswork
            // presented as a feature.
            not_yet("import", 7, &format!("importing from {from:?}"))
        }
        // `open` is the TUI with a bookmark pre-selected, so it shares the
        // path rather than duplicating the startup sequence.
        Some(Command::Open { .. }) | None => run_tui(&cli),
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
    if !tui::is_interactive() {
        anyhow::bail!("lazysync needs a terminal; use `lazysync sync <profile>` for cron");
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let config = Config::load_default(cli.config.as_deref())?;

    // Keep the guard alive for the whole run: dropping it stops the
    // background log writer, which would silently lose every later line.
    let _log_guard = tui::init_file_logging("lazysync=warn");

    let (left, right) = cli.pane_targets(&cwd);

    // A URL in either position names a connection rather than a directory.
    // Parsed here so a typo is reported before the terminal is taken over --
    // an error written over the alternate screen is unreadable.
    let remote = cli::looks_like_url(&right)
        .then(|| connection::parse(&right))
        .transpose()?;
    if cli::looks_like_url(&left) {
        anyhow::bail!(
            "the left pane is the local side; pass {left:?} as the second argument instead"
        );
    }

    let local_start = match &remote {
        // With a remote on the right, the left pane is the working directory
        // unless something else was asked for.
        Some(_) if cli.targets.len() < 2 => cwd.clone(),
        _ => PathBuf::from(&left),
    };

    let mut app = App::new(
        config,
        local_start,
        // The right pane starts local and is replaced once connected, so the
        // browser never has to handle a pane that does not exist.
        if remote.is_some() {
            cwd.clone()
        } else {
            PathBuf::from(&right)
        },
    )?;

    match (&cli.command, remote) {
        // `lazysync open <name>`
        (Some(Command::Open { name }), _) => {
            let Some(bookmark) = app.store.get(name).cloned() else {
                let known: Vec<_> = app
                    .store
                    .bookmarks
                    .iter()
                    .map(|b| b.name.as_str())
                    .collect();
                anyhow::bail!(
                    "no bookmark called {name:?}{}",
                    if known.is_empty() {
                        ". There are no saved connections yet".to_string()
                    } else {
                        format!(" (have: {})", known.join(", "))
                    }
                );
            };
            app.begin_open(&bookmark);
        }

        // `lazysync sftp://host/path`
        (_, Some(target)) => {
            let name = target.display();
            let mut bookmark = lazysync::bookmarks::Bookmark::from_target(&name, &target);
            bookmark.local_path = Some(local_start_of(&app));
            app.begin_open(&bookmark);
        }

        // No connection asked for. Open the bookmark list unless paths were
        // given, in which case the user clearly wants those two directories.
        (_, None) => {
            if cli.targets.is_empty() && should_auto_open(cli, &app) {
                open_from_working_directory(&mut app, &cwd);
            } else if cli.targets.is_empty() {
                app.start_on_bookmarks();
            }
        }
    }

    tui::run(&mut app)
}

fn local_start_of(app: &App) -> PathBuf {
    app.panes[0].real_path().to_path_buf()
}

/// Whether the zide auto-open behaviour applies.
fn should_auto_open(cli: &Cli, app: &App) -> bool {
    if cli.no_auto_open {
        return false;
    }
    cli.auto_open || app.config.zide.auto_open_bookmark
}

/// Open the bookmark whose local path contains the working directory.
///
/// This is the zide popup case: lazysync already knows where the pane started,
/// so making the user pick from a list is a wasted keystroke. Several equally
/// deep matches are ambiguous, so those fall back to the list -- filtered,
/// rather than guessed between.
fn open_from_working_directory(app: &mut App, cwd: &std::path::Path) {
    use lazysync::integrations::zide;

    let policy = app.config.zide.r#match;

    if let Some((bookmark, found)) = zide::single(&app.store, cwd, policy) {
        let bookmark = bookmark.clone();
        let relative = found.relative.clone();

        tracing::info!(
            "working directory selected the bookmark {:?}{}",
            bookmark.name,
            if relative.as_os_str().is_empty() {
                String::new()
            } else {
                format!(" at {}", relative.display())
            }
        );

        // Pre-position both panes at the subdirectory the popup was opened
        // from, which is the whole point of matching an ancestor rather than
        // only an exact path.
        let mut adjusted = bookmark.clone();
        if !relative.as_os_str().is_empty() {
            if let Some(local) = &bookmark.local_path {
                adjusted.local_path = Some(local.join(&relative));
            }
            adjusted.remote_path = bookmark
                .remote_path
                .as_ref()
                .map(|remote| remote.join(&relative));
        }

        app.begin_open(&adjusted);
        return;
    }

    app.start_on_bookmarks();
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
