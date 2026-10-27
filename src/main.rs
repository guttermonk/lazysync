//! Binary entry point.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use lazysync::{
    app::App,
    cli::{self, Cli, Command},
    config::{self, Config},
    connection,
    keys::{defaults, Mode},
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
        Some(Command::Sync {
            profile,
            source,
            destination,
            dry_run,
            yes,
        }) => {
            init_stderr_logging();
            run_headless_sync(&cli, profile, source, destination, *dry_run, *yes)
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

                // What the mode *cannot* do. An action with no key is silent
                // at runtime -- you press something, nothing happens, and
                // there is no way to tell a missing binding from a broken
                // feature. Every way of editing a keymap can produce one: a
                // `base` table that omits an action, a `set` or `extra` that
                // takes a key another action was using, an `unbind` aimed at
                // a chord prefix. So it is reported here rather than left to
                // be discovered.
                let bound = keymap.actions();
                let missing: Vec<_> = defaults::for_mode(mode)
                    .actions()
                    .into_iter()
                    .filter(|action| !bound.contains(action))
                    .collect();
                if !missing.is_empty() {
                    println!("  # unbound in this mode, but bound by default:");
                    for action in missing {
                        println!("  # {:<18} {}", action.name(), action.describe());
                    }
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

/// `lazysync sync <profile>`, with no terminal.
///
/// Shares the planner and the executor with the interactive path, which is the
/// point: a `--dry-run` report and the plan the confirmation window shows
/// cannot drift apart, because they are produced by the same code from the
/// same inputs.
///
/// Remote profiles are refused for now. Connecting non-interactively needs a
/// credential with nowhere to prompt for it, and the keyring is the answer --
/// but a cron job on a locked session cannot reach the keyring either, so that
/// deserves its own design rather than a half-working version here.
fn run_headless_sync(
    cli: &Cli,
    profile_name: &str,
    source_arg: &std::path::Path,
    dest_arg: &std::path::Path,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    use lazysync::{
        sync::{audit, diff, executor, safety, transfer::Settings as TransferSettings},
        vfs::LocalFs,
    };

    let config = Config::load_default(cli.config.as_deref())?;

    let profile = config.profiles.get(profile_name).with_context(|| {
        let known: Vec<_> = config.profiles.keys().map(String::as_str).collect();
        if known.is_empty() {
            format!("no sync profile called {profile_name:?}; config.toml has none defined")
        } else {
            format!(
                "no sync profile called {profile_name:?} (have: {})",
                known.join(", ")
            )
        }
    })?;

    if !profile.mode.is_implemented() {
        anyhow::bail!(
            "the {} mode arrives in a later phase -- see docs/PLAN.md",
            profile.mode.label()
        );
    }

    // Bisync headlessly is refused, and not for want of a planner: the two-way
    // planner is right here. What it lacks is anywhere to resolve a conflict.
    // The options would be to pick a side automatically -- which is the one
    // thing this design refuses to do, since it discards somebody's edit
    // silently -- or to abort the whole run on the first conflict, which makes
    // a cron job that works until the day it matters. Both are worse than
    // saying so.
    if matches!(profile.mode, lazysync::config::SyncMode::Bisync) {
        anyhow::bail!(
            "bisync needs a terminal: a conflict has no safe automatic answer, and resolving \
             one is the whole point. Run it interactively, or use a one-directional profile \
             here"
        );
    }

    // Local paths only, for now.
    for target in [source_arg, dest_arg] {
        let shown = target.display().to_string();
        if cli::looks_like_url(&shown) {
            anyhow::bail!(
                "headless syncing to {shown:?} needs credentials with nowhere to prompt for \
                 them; local paths only for now -- see docs/PLAN.md"
            );
        }
    }

    if !source_arg.is_dir() {
        anyhow::bail!("{} is not a directory", source_arg.display());
    }
    if !dest_arg.is_dir() {
        anyhow::bail!("{} is not a directory", dest_arg.display());
    }

    // `source` and `destination` are named from the user's point of view, so a
    // pull profile means the first argument is where files *go*. Swapping here
    // rather than in the planner keeps the argument names honest.
    let direction = profile.mode.direction();
    let (source_root, dest_root) = if direction.source == 0 {
        (source_arg.to_path_buf(), dest_arg.to_path_buf())
    } else {
        (dest_arg.to_path_buf(), source_arg.to_path_buf())
    };

    let mut source = LocalFs::new();
    let mut dest = LocalFs::new();

    let mut plan = diff::plan(
        diff::Endpoint {
            fs: &mut source,
            root: &source_root,
            label: "source".into(),
        },
        diff::Endpoint {
            fs: &mut dest,
            root: &dest_root,
            label: "destination".into(),
        },
        direction,
        &config,
    );
    safety::check(&mut plan, &config.safety);

    print_plan(&plan, profile.mode.label());

    if let Some(refusal) = &plan.refusal {
        // Logged before bailing. A refusal is the most interesting thing a
        // cron run can produce -- it means a sync was stopped from doing
        // something unwanted -- and without a record the only evidence is a
        // non-zero exit nobody was watching for.
        let _ = audit::append(&audit::Entry::refusal(
            chrono::Utc::now(),
            profile.mode.label(),
            &source_root.display().to_string(),
            &dest_root.display().to_string(),
            refusal,
        ));
        anyhow::bail!("{refusal}");
    }
    if dry_run {
        return Ok(());
    }
    if !yes {
        anyhow::bail!(
            "refusing to apply without --yes. Run with --dry-run to see the plan, or --yes to \
             apply it"
        );
    }
    if !plan.can_apply() {
        println!("nothing to do");
        return Ok(());
    }

    // `apply` takes the two endpoints as panes, because each row names the
    // side it writes to. Pane 0 is the first argument and pane 1 the second,
    // whichever way the profile's direction runs -- the swap above already
    // accounted for that when choosing which root the planner called the
    // source, and applying it twice would send a pull backwards.
    let report = executor::apply(
        &mut source,
        source_arg,
        &mut dest,
        dest_arg,
        &plan,
        &TransferSettings::from_config(&config),
    );

    // `--yes` means nobody reviewed this, which is the distinction the log
    // exists to record.
    if let Err(e) = audit::append(&audit::Entry::from_report(
        chrono::Utc::now(),
        profile.mode.label(),
        "native",
        &source_root.display().to_string(),
        &dest_root.display().to_string(),
        false,
        &report,
    )) {
        eprintln!("the audit log could not be written: {e}");
    }

    for failure in report.failures() {
        eprintln!(
            "failed: {} -- {:?}",
            failure.path.display(),
            failure.outcome
        );
    }
    for skipped in report.skipped() {
        eprintln!(
            "skipped: {} -- it changed underneath",
            skipped.path.display()
        );
    }
    println!("{}", report.summary());

    // A non-zero exit when anything failed, so a cron job or a systemd timer
    // notices rather than reporting success over a partial sync.
    if report.failures().count() > 0 {
        anyhow::bail!("{} operation(s) failed", report.failures().count());
    }

    Ok(())
}

/// Print a plan as text, for the headless path.
///
/// The same information the confirmation window shows, in the same order --
/// both sides' size and time per row, and the destructive sections above the
/// skipped list.
fn print_plan(plan: &lazysync::sync::Plan, mode: &str) {
    use lazysync::{sync::plan::Section, ui::format_size};

    println!(
        "{mode}: {} -> {}  (compare: {}, tolerance {}s)",
        plan.source_root.display(),
        plan.dest_root.display(),
        plan.method.describe(),
        plan.tolerance_secs
    );

    for section in Section::ALL {
        let rows: Vec<_> = plan.section(section).collect();
        if rows.is_empty() {
            continue;
        }
        println!("\n{} ({})", section.title(), rows.len());

        for row in rows {
            let describe = |side: Option<lazysync::sync::plan::Side>| match side {
                Some(s) => format!(
                    "{:>9}  {}",
                    format_size(s.size),
                    s.mtime
                        .map_or_else(|| "-".into(), |t| t.format("%Y-%m-%d %H:%M").to_string())
                ),
                None => format!("{:>9}  {:16}", "-", "-"),
            };

            println!(
                "  {} {:<40} {}   {}   {}{}",
                row.action.glyph(),
                row.path.display(),
                describe(row.source),
                describe(row.dest),
                row.reason.describe(),
                row.caveat.map(|c| format!("  [{c}]")).unwrap_or_default(),
            );
        }
    }

    for (path, why) in &plan.unreadable {
        eprintln!("unreadable: {} -- {why}", path.display());
    }

    let totals = plan.totals();
    println!(
        "\n{} ops, {} out, {} deleted ({}), {} dirs, {} unchanged",
        totals.operations(),
        format_size(totals.bytes_out),
        totals.deletes,
        format_size(totals.bytes_deleted),
        totals.directories,
        totals.skipped,
    );
    if totals.clobbers_newer > 0 {
        println!(
            "WARNING: {} row(s) would overwrite NEWER data with older",
            totals.clobbers_newer
        );
    }
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
