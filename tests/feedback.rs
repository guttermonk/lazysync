//! Telling the user that something landed, and that something is running.
//!
//! Two complaints, one cause. Pressing `s` showed nothing, because `s` alone
//! is bound to nothing and nothing echoed it. Pressing `sp` then froze the
//! terminal for as long as the scan took, because planning walks both trees
//! inside one call and the loop cannot draw while it does -- so the
//! "planning push..." that `begin_sync` has always set arrived only once it
//! was no longer true.
//!
//! The harness mirrors `tui::main_loop`, including the line that copies the
//! dispatcher's partial chord into `App`: these are tests about the wiring,
//! so the wiring is what they drive.

use std::{fs, time::Duration};

use lazysync::{
    app::{App, Screen},
    bookmarks::{Bookmark, BookmarkStore},
    config::Config,
    event::{Dispatch, Dispatcher},
    keys::{Action, Key, ModifiedKey},
    sync::progress::{Flow, Progress, Step},
};
use ratatui::{backend::TestBackend, Terminal};
use tempfile::TempDir;

struct Harness {
    app: App,
    dispatcher: Dispatcher,
    terminal: Terminal<TestBackend>,
    dir: TempDir,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("left")).unwrap();
        fs::create_dir_all(dir.path().join("right")).unwrap();

        let app = App::new(
            Config::default(),
            dir.path().join("left"),
            dir.path().join("right"),
        )
        .unwrap();

        Self {
            app,
            dispatcher: Dispatcher::new(Duration::from_millis(500)),
            terminal: Terminal::new(TestBackend::new(110, 12)).unwrap(),
            dir,
        }
    }

    /// One character, dispatched the way the event loop dispatches it.
    fn press(&mut self, ch: char) {
        self.key(ModifiedKey::plain(Key::Char(ch)));
    }

    fn key(&mut self, key: ModifiedKey) {
        let mode = self.app.mode();

        match self.dispatcher.key(mode, &self.app.keymaps, key) {
            Dispatch::Act(action) => self.app.handle(action),
            Dispatch::Text(c) => self.app.handle_text(c),
            Dispatch::Backspace => self.app.handle_backspace(),
            Dispatch::Pending | Dispatch::Unbound => {}
        }

        self.sync_pending();
    }

    /// Let the chord timeout expire, as the loop does when no key follows.
    fn time_out(&mut self) {
        if let Some(action) = self.dispatcher.timed_out() {
            self.app.handle(action);
        }
        self.sync_pending();
    }

    /// The line `tui::main_loop` runs after every event: the status line
    /// draws from `App`, never from the dispatcher.
    fn sync_pending(&mut self) {
        self.app.pending_keys.clear();
        self.app
            .pending_keys
            .extend_from_slice(self.dispatcher.partial());
    }

    fn screen(&mut self) -> String {
        let app = &mut self.app;
        self.terminal
            .draw(|frame| lazysync::ui::draw(frame, app))
            .unwrap();

        let buffer = self.terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ── The chord echo ───────────────────────────────────────────────────────

/// The reported bug: `sp` is a sync, `s` is half of one, and pressing `s`
/// gave no sign that the key had arrived.
#[test]
fn a_half_typed_chord_is_on_screen() {
    let mut h = Harness::new();
    h.press('s');

    assert_eq!(h.app.pending_keys.len(), 1, "the chord is in progress");

    let screen = h.screen();
    assert!(
        screen.contains("sp push"),
        "the bar should offer what `s` can still become:\n{screen}"
    );
    assert!(
        screen.contains("sb bisync"),
        "every continuation, not just the first:\n{screen}"
    );

    let status = screen.lines().last().expect("a status line");
    assert!(
        status.trim_end().ends_with('s'),
        "the key itself is echoed in the corner, vim-style: {status:?}"
    );
}

/// And the second key resolves it, leaving nothing behind.
#[test]
fn completing_a_chord_clears_the_echo() {
    let mut h = Harness::new();
    h.press('s');
    h.press('p');

    assert!(h.app.pending_keys.is_empty());

    let screen = h.screen();
    assert!(
        screen.contains("? help"),
        "the bar goes back to the normal hints:\n{screen}"
    );
}

/// A chord the loop *does* time out -- which with the stock keymap is none
/// of them, but a keymap that binds `d` alongside `dd` still has the clock --
/// must not leave a stale `s` sitting in the corner.
#[test]
fn an_abandoned_chord_clears_when_it_times_out() {
    let mut h = Harness::new();
    h.press('s');
    assert!(!h.app.pending_keys.is_empty());

    h.time_out();
    assert!(h.app.pending_keys.is_empty());

    let screen = h.screen();
    assert!(screen.contains("? help"), "{screen}");
}

/// A key that resolves on its own never looks like a chord.
#[test]
fn a_single_key_binding_leaves_nothing_pending() {
    let mut h = Harness::new();
    h.press('j');
    assert!(h.app.pending_keys.is_empty());
}

// ── Backing out ──────────────────────────────────────────────────────────

/// Starting `sp` by mistake used to have no answer: the only ways out were
/// to wait out the timeout or to press something that fired instead.
#[test]
fn the_cancel_key_abandons_a_chord_without_doing_anything_else() {
    let mut h = Harness::new();
    let before = h.app.panes[0].path().to_path_buf();

    h.press('s');
    h.key(ModifiedKey::plain(Key::Esc));

    assert!(h.app.pending_keys.is_empty(), "the chord is gone");
    assert_eq!(
        h.app.screen,
        Screen::Browser,
        "and the screen is still here"
    );
    assert_eq!(h.app.panes[0].path(), before);
    assert!(h.app.plan.is_none(), "nothing was planned");

    let screen = h.screen();
    assert!(
        screen.contains("? help"),
        "the bar is back to normal:\n{screen}"
    );
}

/// The multiplexer case, end to end: Esc belongs to zellij, so `ctrl+c` is
/// the cancel key -- and it has to back out of a chord as well.
#[test]
fn a_keymap_that_gave_esc_away_still_has_a_way_out() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("left")).unwrap();
    fs::create_dir_all(dir.path().join("right")).unwrap();

    let config: Config = toml::from_str(
        r#"
[keys.browser]
unbind = ["esc"]
"#,
    )
    .unwrap();

    let mut h = Harness {
        app: App::new(config, dir.path().join("left"), dir.path().join("right")).unwrap(),
        dispatcher: Dispatcher::new(Duration::from_millis(500)),
        terminal: Terminal::new(TestBackend::new(110, 12)).unwrap(),
        dir,
    };

    h.press('s');
    let screen = h.screen();
    assert!(
        screen.contains("ctrl+c back"),
        "the bar must name the key this keymap actually has:\n{screen}"
    );

    h.key(ModifiedKey::new(
        Key::Char('c'),
        lazysync::keys::Mods::ctrl(),
    ));
    assert!(h.app.pending_keys.is_empty());
    assert!(h.app.plan.is_none());
}

// ── Announcing work ──────────────────────────────────────────────────────

/// `begin_sync` has always set this message. The event loop now draws a
/// frame carrying it *before* dispatching, so it is on screen during the
/// scan rather than after it.
#[test]
fn a_sync_says_what_it_is_doing_before_it_blocks() {
    let mut h = Harness::new();

    assert!(h.app.announce_work(Action::SyncPush));
    assert_eq!(h.app.status, "planning push...");

    let screen = h.screen();
    assert!(screen.contains("planning push..."), "{screen}");
}

#[test]
fn every_sync_direction_announces_itself() {
    let mut h = Harness::new();

    for (action, expected) in [
        (Action::SyncPull, "planning pull..."),
        (Action::SyncMirror, "planning mirror..."),
        (Action::SyncBisync, "planning bisync..."),
    ] {
        assert!(h.app.announce_work(action), "{action} said nothing");
        assert_eq!(h.app.status, expected);
    }
}

/// Opening a connection blocks on the network, and "opening..." on a list of
/// connections leaves the one useful fact out.
#[test]
fn opening_a_connection_names_the_one_being_opened() {
    let mut h = Harness::new();
    h.app.screen = Screen::Bookmarks;
    h.app.store = BookmarkStore {
        bookmarks: vec![Bookmark {
            name: "web-server".into(),
            ..Bookmark::local_transfer()
        }],
        ..BookmarkStore::default()
    };

    assert!(h.app.announce_work(Action::OpenItem));
    assert_eq!(h.app.status, "opening web-server...");
}

/// Only the slow ones. A "moving down..." frame before every cursor move
/// would be a redraw per keystroke and a status line nobody could read.
#[test]
fn an_instant_action_announces_nothing() {
    let mut h = Harness::new();

    for action in [
        Action::MoveDown,
        Action::ToggleMark,
        Action::NextPane,
        Action::Help,
    ] {
        assert!(
            !h.app.announce_work(action),
            "{action} is instant and should say nothing"
        );
    }
}

/// The same action means something else in another mode, so the message is
/// keyed on both: `EnterDir` opens a connection on the bookmark list and
/// walks into a directory in the browser.
#[test]
fn the_message_depends_on_the_screen_it_is_pressed_on() {
    let mut h = Harness::new();

    assert!(
        !h.app.announce_work(Action::EnterDir),
        "entering a directory in the browser is one listing, not a scan"
    );

    h.app.screen = Screen::Bookmarks;
    h.app.store = BookmarkStore {
        bookmarks: vec![Bookmark {
            name: "web-server".into(),
            ..Bookmark::local_transfer()
        }],
        ..BookmarkStore::default()
    };

    assert!(h.app.announce_work(Action::EnterDir));
    assert_eq!(h.app.status, "opening web-server...");
}

// ── Stopping a scan ──────────────────────────────────────────────────────

/// Stops the walk at the first thing it is told about.
struct StopAtOnce {
    stopped: usize,
}

impl Progress for StopAtOnce {
    fn step(&mut self, _step: Step<'_>) -> Flow {
        self.stopped += 1;
        Flow::Cancel
    }
}

/// A scan the user stops leaves the browser exactly as it was -- and says so,
/// because a sync key that silently did nothing reads as a key that failed.
#[test]
fn a_cancelled_scan_opens_no_plan_and_explains_itself() {
    let mut h = Harness::new();
    fs::write(h.dir.path().join("left/fresh.txt"), "new").unwrap();
    for pane in &mut h.app.panes {
        pane.reload();
    }

    h.app.progress = Box::new(StopAtOnce { stopped: 0 });
    h.app.handle(Action::SyncPush);

    assert_eq!(h.app.screen, Screen::Browser, "no window opened");
    assert!(h.app.plan.is_none(), "no plan was kept");
    assert!(
        h.app.status.contains("cancelled"),
        "the status should say what happened: {:?}",
        h.app.status
    );
}

/// The sink is lent to the walk and given back. Without that, the second
/// sync of a session would report to nothing -- no spinner, and no way to
/// stop it.
#[test]
fn the_progress_sink_survives_the_scan_it_was_lent_to() {
    let mut h = Harness::new();
    fs::write(h.dir.path().join("left/fresh.txt"), "new").unwrap();
    for pane in &mut h.app.panes {
        pane.reload();
    }

    h.app.progress = Box::new(StopAtOnce { stopped: 0 });

    h.app.handle(Action::SyncPush);
    assert!(h.app.plan.is_none());

    // Again. If the sink had not been put back, this would plan normally and
    // open the confirmation window.
    h.app.handle(Action::SyncPush);
    assert!(
        h.app.plan.is_none(),
        "the second scan reported to nothing, so it could not be stopped"
    );
    assert_eq!(h.app.screen, Screen::Browser);
}

/// Belt and braces for the harness itself: both panes are under the temp
/// directory, so nothing here touches a real tree.
#[test]
fn the_harness_stays_in_its_temp_directory() {
    let h = Harness::new();
    assert!(h.app.panes[0].path().starts_with(h.dir.path()));
    assert!(h.app.panes[1].path().starts_with(h.dir.path()));
}
