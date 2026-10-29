//! Terminal setup and the main loop.

use std::{
    io,
    time::{Duration, Instant},
};

use anyhow::Result;
use ratatui::{
    crossterm::{
        cursor,
        event::{self, DisableMouseCapture, Event, MouseButton, MouseEventKind},
        execute, queue, terminal,
    },
    DefaultTerminal,
};

use crate::{
    app::App,
    config::Palette,
    event::{Dispatch, Dispatcher},
    keys::{Action, Keymaps, Mode, ModifiedKey},
    sync::progress::{Flow, Progress, Step},
    ui::{self, Target},
};

/// How close together two presses on the same thing have to be to count as a
/// double click. Terminals do not report double clicks, so lazysync times
/// them itself.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Run the browser until the user quits.
pub fn run(app: &mut App) -> Result<()> {
    // `ratatui::init` installs its own panic hook that restores the terminal
    // before unwinding, so a panic leaves a usable shell rather than a raw-mode
    // terminal with no echo.
    let mut terminal = ratatui::init();

    // Where a tree walk reports to. `App` carries it so that planning can
    // reach it from inside the borrow it holds on both panes; it defaults to
    // reporting nothing, which is what every headless path keeps.
    app.progress = Box::new(ScanSpinner::new(app.palette, &app.keymaps));

    let mouse = app.config.mouse && enable_mouse();
    let result = main_loop(&mut terminal, app);
    if mouse {
        disable_mouse();
    }

    ratatui::restore();
    result
}

/// Turn on button reporting (`?1000`) with SGR coordinates (`?1006`).
///
/// Written out rather than using crossterm's `EnableMouseCapture`, which also
/// turns on `?1002` and `?1003`: motion reporting. lazysync does not drag
/// anything, and with `?1003` on, the terminal sends an event for every cell
/// the pointer crosses -- each of which wakes the loop below and redraws the
/// whole frame. The main loop is built to block on input and use no CPU while
/// idle, and nothing should undo that just by the mouse passing over the
/// window. `?1000` still reports the wheel, which is the other half of this.
const ENABLE_MOUSE: &str = "\x1b[?1000h\x1b[?1006h";

/// Ask the terminal to report clicks and the wheel.
///
/// Mouse reporting is a terminal mode rather than a ratatui one, so it is
/// turned on here and has to be turned off again on *both* ways out: the
/// normal one, and the panic. Ratatui's own hook knows nothing about it, and
/// a terminal left reporting mouse after a crash spits escape sequences at
/// every twitch of the hand.
fn enable_mouse() -> bool {
    use io::Write;

    let mut stdout = io::stdout();
    if stdout
        .write_all(ENABLE_MOUSE.as_bytes())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return false;
    }

    let restore_terminal = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Before ratatui's hook, which leaves the alternate screen.
        disable_mouse();
        restore_terminal(info);
    }));

    true
}

/// Crossterm's own, which turns off every reporting mode there is. Clearing
/// ones that were never set costs nothing, and this way the cleanup cannot
/// miss one.
fn disable_mouse() {
    let _ = execute!(io::stdout(), DisableMouseCapture);
}

/// The frames of the spinner, a dot orbiting a braille cell.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How often the scan line is repainted. Fast enough to read as motion,
/// slow enough that a local tree of ten thousand files is not bottlenecked
/// on `write`.
const TICK: Duration = Duration::from_millis(80);

/// Shows a tree walk's progress on the status row, and lets it be stopped.
///
/// **Why this writes escape sequences instead of rendering a frame.** The
/// walk holds a mutable borrow of both panes for its whole run, so anything
/// it calls back into cannot also have `&mut App` -- and without that there
/// is no way to render the UI. What is possible is overwriting one row of a
/// screen that is otherwise standing still, which is all this does. The loop
/// calls `Terminal::clear` afterwards, so ratatui's idea of the screen and
/// the screen itself agree again before the next frame.
///
/// That trade is worth naming: it is a consequence of planning being one
/// blocking call. A walk that yielded would need none of this, and would get
/// a working browser underneath it rather than a frozen one.
struct ScanSpinner {
    palette: Palette,
    /// What the walk is doing, e.g. `planning push`.
    label: String,
    /// Keys that stop it, and how to spell the first of them.
    cancel: Vec<ModifiedKey>,
    cancel_hint: String,

    directories: usize,
    entries: usize,
    frame: usize,
    painted: Option<Instant>,
    stopped: bool,
}

impl ScanSpinner {
    fn new(palette: Palette, keymaps: &Keymaps) -> Self {
        // The browser's own cancel keys, so the way to stop a scan is the
        // way to cancel anything else -- Esc, or `ctrl+c` for a keymap that
        // gave Esc to its multiplexer.
        let combos = keymaps.get(Mode::Browser).combos_for(Action::Cancel);
        let cancel: Vec<ModifiedKey> = combos
            .iter()
            .filter(|combo| combo.len() == 1)
            .map(|combo| combo.0[0])
            .collect();

        let cancel_hint = cancel.first().map(ToString::to_string).unwrap_or_default();

        Self {
            palette,
            label: String::new(),
            cancel,
            cancel_hint,
            directories: 0,
            entries: 0,
            frame: 0,
            painted: None,
            stopped: false,
        }
    }

    /// Take whatever input has arrived, and report whether it says stop.
    ///
    /// Everything else is dropped on purpose. Keys pressed at a frozen screen
    /// were pressed at a screen the user could not see the state of, and
    /// replaying them afterwards -- which is what happened while they sat in
    /// the terminal's buffer -- moves the cursor somewhere nobody chose.
    fn drain_input(&mut self) -> bool {
        while event::poll(Duration::ZERO).unwrap_or(false) {
            let Ok(Event::Key(key)) = event::read() else {
                continue;
            };
            let Some(key) = ModifiedKey::from_event(key) else {
                continue;
            };
            if self.cancel.contains(&key) {
                self.stopped = true;
            }
        }

        self.stopped
    }

    /// The line as the user reads it.
    ///
    /// Separated from the writing so that what it says can be tested without
    /// a terminal -- the counts are the part that has to be true.
    fn line(&self) -> String {
        let counted = match (self.directories, self.entries) {
            (_, 0) => String::new(),
            (1, entries) => format!("  {entries} entries"),
            (dirs, entries) => format!("  {entries} entries in {dirs} directories"),
        };

        // Left out rather than written without a key, like every other hint:
        // a keymap with no Cancel has no way to stop a scan, and saying so
        // would mean naming a key that does nothing.
        let stop = if self.cancel_hint.is_empty() {
            String::new()
        } else {
            format!("  ({} to stop)", self.cancel_hint)
        };

        format!(
            " {} {}...{counted}{stop} ",
            SPINNER[self.frame % SPINNER.len()],
            self.label,
        )
    }

    fn paint(&mut self) {
        use io::Write;
        use ratatui::{backend::IntoCrossterm, crossterm::style};

        let Ok((_, rows)) = terminal::size() else {
            return;
        };

        let line = self.line();
        let mut out = io::stdout();
        let _ = queue!(
            out,
            cursor::MoveTo(0, rows.saturating_sub(1)),
            terminal::Clear(terminal::ClearType::CurrentLine),
            style::SetBackgroundColor(self.palette.status_bg.into_crossterm()),
            style::SetForegroundColor(self.palette.accent.into_crossterm()),
            style::Print(line),
            style::ResetColor,
        );
        let _ = out.flush();
    }
}

impl Progress for ScanSpinner {
    fn start(&mut self, label: &str) {
        self.label = label.to_string();
        self.directories = 0;
        self.entries = 0;
        self.frame = 0;
        self.stopped = false;
        self.painted = None;
    }

    fn step(&mut self, step: Step<'_>) -> Flow {
        match step {
            Step::Directory(_) => self.directories += 1,
            Step::Entry => self.entries += 1,
        }

        // Rate-limited, because this runs once per entry. The counters above
        // are not: they are what makes the line say something true even when
        // the spinner has not moved.
        if self.painted.is_some_and(|painted| painted.elapsed() < TICK) {
            return Flow::Continue;
        }
        self.painted = Some(Instant::now());
        self.frame += 1;

        if self.drain_input() {
            return Flow::Cancel;
        }

        self.paint();
        Flow::Continue
    }
}

/// The last press, for telling a double click from two single ones.
#[derive(Default)]
struct Clicks {
    last: Option<(Target, Instant)>,
}

impl Clicks {
    /// Record a press and say whether it completes a double click.
    fn register(&mut self, target: Target, now: Instant) -> bool {
        let double = matches!(
            self.last,
            Some((previous, when))
                if previous == target && now.duration_since(when) < DOUBLE_CLICK
        );

        // A double click does not seed a triple: the third press starts over,
        // so holding down a click does not enter a directory repeatedly.
        self.last = if double { None } else { Some((target, now)) };
        double
    }
}

/// When the loop should next wake up, and whose clock it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Deadline {
    remaining: Duration,
    /// Whether the chord's own timeout is the one that will elapse.
    chords: bool,
}

/// The soonest of the deadlines that can be pending.
///
/// Knowing *whose* it is matters. The loop used to take the minimum and then,
/// on waking, abandon whatever chord was in progress -- so with watch mode on,
/// a batch of file changes going quiet would eat a half-typed `sp`. A clock
/// may only expire the thing it belongs to.
///
/// Only the chord's identity needs carrying, because it is the only one that
/// *consumes* state when it fires. The watcher and the keepalive are both
/// checked on every pass of the loop regardless of what woke it, so waking
/// early for the other one costs nothing and waking late is the thing these
/// deadlines exist to prevent.
fn next_deadline(
    chord: Option<Duration>,
    watch: Option<Duration>,
    keepalive: Option<Duration>,
) -> Option<Deadline> {
    let other = match (watch, keepalive) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };

    match (chord, other) {
        (Some(chord), Some(other)) if chord <= other => Some(Deadline {
            remaining: chord,
            chords: true,
        }),
        (_, Some(other)) => Some(Deadline {
            remaining: other,
            chords: false,
        }),
        (Some(chord), None) => Some(Deadline {
            remaining: chord,
            chords: true,
        }),
        (None, None) => None,
    }
}

fn main_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut dispatcher = Dispatcher::new(app.config.chord_timeout());
    let mut clicks = Clicks::default();

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
                handle(terminal, app, &mut dispatcher, &mut clicks, event::read()?)?;
            }
            continue;
        }

        // Watch mode gives the loop a second reason to wake up. Two deadlines
        // can be pending -- a chord timing out and a batch of file changes
        // going quiet -- so the shorter one wins. With neither, the loop still
        // blocks on input, so an idle lazysync uses no CPU even with a
        // watcher running: `notify` does its waiting in its own thread, and
        // a change arriving is what creates a deadline in the first place.
        //
        // The subtlety is that a watcher with nothing pending has *no*
        // deadline. Polling on a fixed interval instead would mean waking
        // several times a second for ever, which is exactly what the blocking
        // read was chosen to avoid.
        match next_deadline(
            dispatcher.poll_timeout(),
            app.watch_deadline(),
            app.keepalive_deadline(),
        ) {
            None => handle(terminal, app, &mut dispatcher, &mut clicks, event::read()?)?,
            Some(Deadline { remaining, chords }) => {
                if event::poll(remaining)? {
                    handle(terminal, app, &mut dispatcher, &mut clicks, event::read()?)?;
                } else if chords {
                    if let Some(action) = dispatcher.timed_out() {
                        act(terminal, app, action)?;
                    }
                }
            }
        }

        // The echo of a chord in progress, which the status line draws. Done
        // here rather than inside `handle` so that a chord abandoned by the
        // timeout above clears it too.
        app.pending_keys.clear();
        app.pending_keys.extend_from_slice(dispatcher.partial());

        // After input, so a keystroke is never delayed by a sync starting --
        // and before the next draw, so the plan a batch opens is on screen in
        // the same frame.
        if app.watcher.is_some() {
            app.step_watch();
        }

        // Last, and on every pass rather than only when its own deadline
        // fired: it is cheap, it checks the clock itself, and a wake-up for
        // any other reason is as good a moment as a wake-up for this one.
        app.tick_keepalive();
    }

    Ok(())
}

/// Run an action, having first drawn a frame for the ones that block.
///
/// Every action goes through here rather than straight to `App::handle`, so
/// that there is exactly one place where "this is about to take a while" is
/// put on screen -- and so the mouse gets it as well as the keyboard, which
/// matters because double-clicking a connection blocks for just as long as
/// pressing Enter on it.
fn act(terminal: &mut DefaultTerminal, app: &mut App, action: Action) -> Result<()> {
    let blocks = app.announce_work(action);
    if blocks {
        terminal.draw(|frame| ui::draw(frame, app))?;
    }

    app.handle(action);

    if blocks {
        // Whatever was typed at a screen that could not redraw is dropped
        // rather than delivered. It would otherwise all arrive at once the
        // moment the scan finished, and a burst of held-down `j` replayed
        // into a pane whose contents have just changed lands the cursor
        // somewhere nobody chose.
        while event::poll(Duration::ZERO)? {
            let _ = event::read()?;
        }

        // The scan paints its own status row behind ratatui's back, so the
        // next frame has to be a full one rather than a diff against a
        // buffer that never knew about it.
        terminal.clear()?;
    }

    Ok(())
}

fn handle(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    dispatcher: &mut Dispatcher,
    clicks: &mut Clicks,
    raw: Event,
) -> Result<()> {
    match raw {
        Event::Key(key) => {
            // `from_event` drops key releases and repeats, which terminals
            // that negotiate the kitty keyboard protocol also send; acting on
            // a release would run every binding twice.
            let Some(key) = ModifiedKey::from_event(key) else {
                return Ok(());
            };

            let mode = app.mode();
            match dispatcher.key(mode, &app.keymaps, key) {
                Dispatch::Act(action) => act(terminal, app, action)?,
                Dispatch::Text(ch) => app.handle_text(ch),
                Dispatch::Backspace => app.handle_backspace(),
                Dispatch::Pending | Dispatch::Unbound => {}
            }
        }
        // A click means whatever was drawn where it landed. The hit map is
        // rebuilt by every frame and this runs straight after one, so it
        // describes the screen in front of the user -- not the one before
        // the last resize.
        Event::Mouse(mouse) => {
            let Some(target) = app.hits.at(mouse.column, mouse.row) else {
                return Ok(());
            };

            match mouse.kind {
                // On the press rather than the release, which is what makes
                // it feel immediate. The release is then ignored, so one
                // click never acts twice.
                MouseEventKind::Down(MouseButton::Left) => {
                    let double = clicks.register(target, Instant::now());
                    if let Some(action) = app.handle_click(target, double) {
                        act(terminal, app, action)?;
                    }
                }
                MouseEventKind::ScrollDown => app.handle_scroll(target, true),
                MouseEventKind::ScrollUp => app.handle_scroll(target, false),
                _ => {}
            }
        }
        // A resize redraws on the next loop iteration anyway; ratatui
        // recalculates the layout from the new frame size.
        Event::Resize(_, _) => {}
        _ => {}
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Target = Target::Pane {
        pane: 0,
        row: Some(3),
    };
    const RIGHT: Target = Target::Pane {
        pane: 1,
        row: Some(3),
    };

    #[test]
    fn two_quick_presses_on_the_same_thing_are_a_double_click() {
        let mut clicks = Clicks::default();
        let start = Instant::now();

        assert!(!clicks.register(LEFT, start));
        assert!(clicks.register(LEFT, start + Duration::from_millis(100)));
    }

    #[test]
    fn a_slow_second_press_is_two_single_clicks() {
        let mut clicks = Clicks::default();
        let start = Instant::now();

        assert!(!clicks.register(LEFT, start));
        assert!(!clicks.register(LEFT, start + DOUBLE_CLICK + Duration::from_millis(1)));
    }

    /// Clicking one row and then quickly another is not a double click on the
    /// second -- it is two people's worth of hurry and one selection each.
    #[test]
    fn a_quick_press_somewhere_else_is_not_a_double_click() {
        let mut clicks = Clicks::default();
        let start = Instant::now();

        assert!(!clicks.register(LEFT, start));
        assert!(!clicks.register(RIGHT, start + Duration::from_millis(50)));
    }

    fn spinner() -> ScanSpinner {
        let palette = crate::config::Theme::default().palette().unwrap();
        ScanSpinner::new(palette, &Keymaps::default())
    }

    /// What the line says has to be true: the counts are the evidence that
    /// something is happening, and the spinner alone cannot carry that.
    #[test]
    fn the_scan_line_reports_what_has_been_walked() {
        let mut spinner = spinner();
        spinner.start("planning push");

        assert!(
            spinner.line().contains("planning push..."),
            "{}",
            spinner.line()
        );

        spinner.directories = 37;
        spinner.entries = 418;
        let line = spinner.line();

        assert!(line.contains("418 entries in 37 directories"), "{line}");
        assert!(line.contains("to stop"), "the way out is named: {line}");
    }

    /// Nothing walked yet is not "0 entries": a count of zero on screen is a
    /// claim, and at that point nothing has been counted.
    #[test]
    fn a_scan_that_has_not_counted_anything_says_nothing_about_counts() {
        let mut spinner = spinner();
        spinner.start("planning pull");

        let line = spinner.line();
        assert!(!line.contains("entries"), "{line}");
        assert!(line.contains("planning pull..."), "{line}");
    }

    /// One directory is the common case for a flat tree, and "in 1
    /// directories" is the kind of thing that makes a program look unfinished.
    #[test]
    fn a_single_directory_is_not_pluralised_badly() {
        let mut spinner = spinner();
        spinner.start("planning push");
        spinner.directories = 1;
        spinner.entries = 9;

        let line = spinner.line();
        assert!(line.contains("9 entries"), "{line}");
        assert!(!line.contains("directories"), "{line}");
    }

    /// The glyph has to move, or it is a picture of a spinner rather than one.
    #[test]
    fn the_spinner_advances_frame_by_frame() {
        let mut spinner = spinner();
        spinner.start("planning push");

        let first = spinner.line();
        spinner.frame += 1;
        assert_ne!(first, spinner.line());

        // And it wraps rather than running off the end of the frame list.
        spinner.frame = SPINNER.len() * 3;
        assert_eq!(spinner.line(), first);
    }

    /// A keymap with no cancel key is offered no way to stop, rather than
    /// being told to press something that does nothing.
    #[test]
    fn a_scan_with_no_cancel_key_does_not_offer_one() {
        use crate::keys::{Keymap, KeymapSection};
        use std::collections::BTreeMap;

        let maps = Keymaps::resolve(&BTreeMap::from([(
            Mode::Browser,
            KeymapSection {
                base: Some(Keymap(Vec::new())),
                ..Default::default()
            },
        )]));

        let palette = crate::config::Theme::default().palette().unwrap();
        let mut spinner = ScanSpinner::new(palette, &maps);
        spinner.start("planning push");

        assert!(!spinner.line().contains("to stop"), "{}", spinner.line());
    }

    /// A chord in progress must not be abandoned because the *watcher's*
    /// deadline came first. Before this, watch mode plus a half-typed `sp`
    /// meant the chord vanished when a batch of file changes went quiet.
    #[test]
    fn a_watch_deadline_does_not_expire_a_chord() {
        let chord = Duration::from_millis(500);
        let watch = Duration::from_millis(100);

        let deadline = next_deadline(Some(chord), Some(watch), None).expect("a deadline");
        assert_eq!(deadline.remaining, watch, "the sooner one still wins");
        assert!(
            !deadline.chords,
            "waking for the watcher must not expire the chord"
        );
    }

    /// The keepalive is a third clock, and it must not expire a chord either.
    #[test]
    fn a_keepalive_deadline_does_not_expire_a_chord() {
        let chord = Duration::from_secs(1);
        let keepalive = Duration::from_millis(50);

        let deadline = next_deadline(Some(chord), None, Some(keepalive)).expect("a deadline");
        assert_eq!(deadline.remaining, keepalive);
        assert!(
            !deadline.chords,
            "waking to ping the server must not throw away a half-typed chord"
        );
    }

    /// Whichever of the two non-chord clocks is sooner, the loop wakes for it
    /// -- both are re-checked on every pass, so waking early is free.
    #[test]
    fn the_sooner_of_the_watcher_and_the_keepalive_wins() {
        let soon = Duration::from_millis(10);
        let later = Duration::from_secs(60);

        let watch_first = next_deadline(None, Some(soon), Some(later)).unwrap();
        assert_eq!(watch_first.remaining, soon);

        let keepalive_first = next_deadline(None, Some(later), Some(soon)).unwrap();
        assert_eq!(keepalive_first.remaining, soon);

        let only_keepalive = next_deadline(None, None, Some(soon)).unwrap();
        assert_eq!(only_keepalive.remaining, soon);
        assert!(!only_keepalive.chords);
    }

    #[test]
    fn the_sooner_clock_wins_and_owns_the_wakeup() {
        let soon = Duration::from_millis(10);
        let later = Duration::from_millis(900);

        let chord_first = next_deadline(Some(soon), Some(later), None).unwrap();
        assert_eq!(chord_first.remaining, soon);
        assert!(chord_first.chords);

        let only_chord = next_deadline(Some(soon), None, None).unwrap();
        assert!(only_chord.chords);

        let only_watch = next_deadline(None, Some(soon), None).unwrap();
        assert!(!only_watch.chords);

        assert_eq!(
            next_deadline(None, None, None),
            None,
            "nothing to wake up for"
        );
    }

    /// Three fast presses act once, not twice. Otherwise a hand that bounces
    /// on the button walks into two directories.
    #[test]
    fn a_third_press_starts_over_rather_than_acting_again() {
        let mut clicks = Clicks::default();
        let start = Instant::now();

        assert!(!clicks.register(LEFT, start));
        assert!(clicks.register(LEFT, start + Duration::from_millis(50)));
        assert!(!clicks.register(LEFT, start + Duration::from_millis(100)));
    }
}
