//! Terminal setup and the main loop.

use std::{io, time::Duration};

use anyhow::Result;
use ratatui::{
    crossterm::event::{self, Event},
    DefaultTerminal,
};

use crate::{
    app::App,
    event::{Dispatch, Dispatcher},
    keys::ModifiedKey,
    ui,
};

/// Run the browser until the user quits.
pub fn run(app: &mut App) -> Result<()> {
    // `ratatui::init` installs its own panic hook that restores the terminal
    // before unwinding, so a panic leaves a usable shell rather than a raw-mode
    // terminal with no echo.
    let mut terminal = ratatui::init();
    let result = main_loop(&mut terminal, app);
    ratatui::restore();
    result
}

fn main_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut dispatcher = Dispatcher::new(app.config.chord_timeout());

    while !app.should_quit {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // A transfer in flight is driven here, a chunk per frame. That is what
        // makes the progress bar move and `esc` work at all: applying a whole
        // plan in one call would block this loop until it finished.
        if app.transfer_is_active() {
            app.step_transfer();

            // Drain whatever input has arrived without waiting for more, so a
            // keypress is noticed within a chunk rather than at the end of the
            // transfer.
            while event::poll(Duration::ZERO)? {
                handle(app, &mut dispatcher, event::read()?);
            }
            continue;
        }

        // With no chord in progress and nothing transferring, there is nothing
        // to wake up for: block on input rather than polling, so an idle
        // lazysync uses no CPU.
        match dispatcher.poll_timeout() {
            None => handle(app, &mut dispatcher, event::read()?),
            Some(remaining) => {
                if event::poll(remaining)? {
                    handle(app, &mut dispatcher, event::read()?);
                } else if let Some(action) = dispatcher.timed_out() {
                    app.handle(action);
                }
            }
        }
    }

    Ok(())
}

fn handle(app: &mut App, dispatcher: &mut Dispatcher, raw: Event) {
    match raw {
        Event::Key(key) => {
            // `from_event` drops key releases and repeats, which terminals
            // that negotiate the kitty keyboard protocol also send; acting on
            // a release would run every binding twice.
            let Some(key) = ModifiedKey::from_event(key) else {
                return;
            };

            let mode = app.mode();
            match dispatcher.key(mode, &app.keymaps, key) {
                Dispatch::Act(action) => app.handle(action),
                Dispatch::Text(ch) => app.handle_text(ch),
                Dispatch::Backspace => app.handle_backspace(),
                Dispatch::Pending | Dispatch::Unbound => {}
            }
        }
        // A resize redraws on the next loop iteration anyway; ratatui
        // recalculates the layout from the new frame size.
        Event::Resize(_, _) => {}
        _ => {}
    }
}

/// Send `tracing` output to a file.
///
/// Logging to stderr is not an option once the alternate screen is up: a
/// single log line would land in the middle of the UI and corrupt it. The file
/// is created lazily, so a read-only state directory degrades to no logging
/// instead of refusing to start.
pub fn init_file_logging(level: &str) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, EnvFilter};

    let dir = crate::config::state_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("no log directory: {e}");
        return None;
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("lazysync.log"))
        .ok()?;

    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_env("LAZYSYNC_LOG").unwrap_or_else(|_| EnvFilter::new(level));

    fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();

    Some(guard)
}

/// Whether stdout is a terminal.
///
/// `lazysync sync --yes` has to work from cron with no tty, so the TUI path is
/// chosen rather than assumed.
pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    io::stdout().is_terminal()
}
