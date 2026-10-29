//! Clicking, over a really rendered frame.
//!
//! These drive [`App`] the way the event loop does: draw a frame, ask the hit
//! map what is at a column and a row, then hand that to `handle_click`. The
//! aiming is deliberately done by *looking for the text on screen* rather than
//! by computing where a row ought to be -- a test that clicks where it thinks
//! `beta.txt` is would keep passing if the layout moved out from under it,
//! which is the one failure the mouse has that the keyboard does not.
//!
//! The double-click timer itself is a unit test in `tui`, so these pass
//! `double` explicitly and stay about what a click *means*.

use std::{fs, path::Path};

use lazysync::{
    app::{Answer, App, Overlay, Pending, Question, Screen},
    bookmarks::{Bookmark, BookmarkStore},
    config::Config,
    keys::Action,
    sync::plan::Section,
    ui::Target,
};
use ratatui::{backend::TestBackend, Terminal};
use tempfile::TempDir;

/// Wide enough for both panes to show names and dates, tall enough that a
/// forty-entry listing has to scroll.
const WIDTH: u16 = 100;
const HEIGHT: u16 = 24;

struct Harness {
    app: App,
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
            terminal: Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap(),
            dir,
        }
    }

    fn left(&self) -> std::path::PathBuf {
        self.dir.path().join("left")
    }

    fn right(&self) -> std::path::PathBuf {
        self.dir.path().join("right")
    }

    fn write(&self, path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn reload(&mut self) {
        for pane in &mut self.app.panes {
            pane.reload();
        }
    }

    /// Render, which is also what fills the hit map.
    fn draw(&mut self) {
        let app = &mut self.app;
        self.terminal
            .draw(|frame| lazysync::ui::draw(frame, app))
            .unwrap();
    }

    fn screen(&self) -> String {
        let buffer = self.terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| self.line(y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn line(&self, y: u16) -> String {
        let buffer = self.terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    /// Where a piece of text starts on screen, counted in *columns*.
    ///
    /// Not bytes: a pane border is a three-byte box-drawing character, so a
    /// byte offset would aim every click in the right-hand pane three columns
    /// short of where it meant to go.
    fn position_of(&self, needle: &str) -> (u16, u16) {
        let buffer = self.terminal.backend().buffer();
        for y in 0..buffer.area.height {
            let cells: Vec<&str> = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            for start in 0..cells.len() {
                if cells[start..].concat().starts_with(needle) {
                    return (start as u16, y);
                }
            }
        }
        panic!("{needle:?} is not on screen:\n{}", self.screen());
    }

    /// The word drawn at a point, up to the next space. A name can sit flush
    /// against the pane border, so splitting the line on whitespace would
    /// hand back `│file-23.txt`.
    fn word_at(&self, column: u16, row: u16) -> String {
        self.line(row)
            .chars()
            .skip(column as usize)
            .take_while(|c| !c.is_whitespace())
            .collect()
    }

    /// What the last frame drew at a point.
    fn target_at(&self, column: u16, row: u16) -> Option<Target> {
        self.app.hits.at(column, row)
    }

    fn target_on(&self, text: &str) -> Target {
        let (column, row) = self.position_of(text);
        self.target_at(column, row)
            .unwrap_or_else(|| panic!("nothing clickable on {text:?}"))
    }

    /// Click whatever is drawn at this text.
    fn click(&mut self, text: &str) {
        let target = self.target_on(text);
        self.press(target, false);
    }

    fn double_click(&mut self, text: &str) {
        let target = self.target_on(text);
        self.press(target, true);
    }

    /// Click, then run whatever the click asked for -- which is what the
    /// event loop does with the action it hands back.
    fn press(&mut self, target: Target, double: bool) {
        if let Some(action) = self.app.handle_click(target, double) {
            self.app.handle(action);
        }
    }

    fn scroll_on(&mut self, text: &str, down: bool) {
        let target = self.target_on(text);
        self.app.handle_scroll(target, down);
    }

    fn selected(&self, pane: usize) -> String {
        self.app.panes[pane]
            .selected()
            .map(|entry| entry.name.clone())
            .unwrap_or_default()
    }
}

/// A file apiece, named per side so a search for one cannot match the other.
fn two_files(h: &Harness) {
    h.write(&h.left().join("left-one.txt"), "a");
    h.write(&h.left().join("left-two.txt"), "b");
    h.write(&h.right().join("right-one.txt"), "c");
    h.write(&h.right().join("right-two.txt"), "d");
}

// ── The browser ──────────────────────────────────────────────────────────

#[test]
fn a_click_on_a_file_focuses_its_pane_and_selects_it() {
    let mut h = Harness::new();
    two_files(&h);
    h.reload();
    h.draw();

    assert_eq!(h.app.focus, 0, "the local pane starts focused");

    h.click("right-two.txt");

    assert_eq!(h.app.focus, 1, "clicking into a pane is also focusing it");
    assert_eq!(h.selected(1), "right-two.txt");
}

/// The other half of the same rule: a click in the pane you are already in
/// moves the cursor and nothing else.
#[test]
fn a_click_selects_within_the_focused_pane() {
    let mut h = Harness::new();
    two_files(&h);
    h.reload();
    h.draw();

    h.click("left-two.txt");

    assert_eq!(h.app.focus, 0);
    assert_eq!(h.selected(0), "left-two.txt");
}

/// A stray click must not navigate. This is the reason the single/double
/// split exists at all: over SFTP, walking into a directory costs a listing.
#[test]
fn a_single_click_on_a_directory_selects_it_without_entering() {
    let mut h = Harness::new();
    h.write(&h.left().join("sub/inside.txt"), "x");
    h.reload();
    h.draw();

    let before = h.app.panes[0].path().to_path_buf();
    h.click("sub/");

    assert_eq!(h.selected(0), "sub");
    assert_eq!(h.app.panes[0].path(), before, "one click never navigates");
}

#[test]
fn a_double_click_enters_a_directory() {
    let mut h = Harness::new();
    h.write(&h.left().join("sub/inside.txt"), "x");
    h.reload();
    h.draw();

    h.double_click("sub/");

    assert_eq!(h.app.panes[0].path(), h.left().join("sub"));
    assert_eq!(h.selected(0), "inside.txt", "and shows what is in there");
}

/// Clicking the empty half of a pane is still "work in this one". It just has
/// no row to select, so the cursor stays where the keyboard left it.
#[test]
fn a_click_below_the_listing_focuses_the_pane_without_moving_the_cursor() {
    let mut h = Harness::new();
    two_files(&h);
    h.reload();
    h.draw();

    // Two entries, so the lower half of the right-hand pane is empty.
    let target = h
        .target_at(WIDTH - 10, HEIGHT - 6)
        .expect("the pane itself is clickable");
    assert_eq!(target, Target::Pane { pane: 1, row: None });

    h.press(target, false);

    assert_eq!(h.app.focus, 1);
    assert_eq!(
        h.selected(1),
        "right-one.txt",
        "the cursor was not dragged down to the empty space"
    );
}

/// The case the hit map's `offset` exists for. With the listing scrolled, the
/// top visible line is not entry zero, and a click that ignored that would
/// select something a screenful away from the thing under the pointer.
#[test]
fn a_click_on_a_scrolled_listing_selects_what_is_drawn_there() {
    let mut h = Harness::new();
    for index in 0..40 {
        h.write(&h.left().join(format!("file-{index:02}.txt")), "x");
    }
    h.reload();

    h.app.handle(Action::MoveBottom);
    h.draw();

    // Whatever ended up on the top line of the pane after scrolling.
    let (column, row) = h.position_of("file-");
    let name = h.word_at(column, row);

    assert_ne!(name, "file-00.txt", "the listing should have scrolled");

    h.click(&name);
    assert_eq!(h.selected(0), name);
}

// ── The wheel ────────────────────────────────────────────────────────────

/// Scrolling is looking, not moving in. A wheel is easy to nudge, and having
/// it steal focus would quietly change which pane the next sync pushes from.
#[test]
fn the_wheel_scrolls_the_pane_under_the_pointer_without_focusing_it() {
    let mut h = Harness::new();
    for index in 0..40 {
        h.write(&h.right().join(format!("right-{index:02}.txt")), "x");
    }
    h.write(&h.left().join("left-one.txt"), "a");
    h.reload();
    h.draw();

    h.scroll_on("right-00.txt", true);

    assert_eq!(h.app.focus, 0, "the wheel does not change focus");
    assert_eq!(
        h.selected(1),
        "right-03.txt",
        "three lines a notch, in the pane the pointer was over"
    );

    h.draw();
    h.scroll_on("right-03.txt", false);
    assert_eq!(h.selected(1), "right-00.txt");
}

#[test]
fn the_wheel_stops_at_the_ends_of_a_listing() {
    let mut h = Harness::new();
    h.write(&h.left().join("only.txt"), "a");
    h.reload();
    h.draw();

    h.scroll_on("only.txt", false);
    h.scroll_on("only.txt", true);

    assert_eq!(h.selected(0), "only.txt");
}

// ── Overlays ─────────────────────────────────────────────────────────────

/// A dialog owns every key while it is up; the mouse must not be the one
/// input that can reach around it.
#[test]
fn an_overlay_swallows_clicks_meant_for_the_screen_behind_it() {
    let mut h = Harness::new();
    h.write(&h.left().join("sub/inside.txt"), "x");
    h.reload();
    h.draw();

    let (column, row) = h.position_of("sub/");

    h.app.handle(Action::Help);
    h.draw();

    let target = h.target_at(column, row).expect("the shield");
    assert_eq!(target, Target::Blocked);

    h.press(target, true);
    assert_eq!(
        h.app.panes[0].path(),
        h.left(),
        "a double click through the overlay must not have entered anything"
    );
}

#[test]
fn the_wheel_scrolls_the_help_overlay_it_is_over() {
    let mut h = Harness::new();
    h.app.handle(Action::Help);
    h.draw();

    h.scroll_on("Motion", true);

    match h.app.overlay {
        Overlay::Help { scroll } => assert_eq!(scroll, 3),
        ref other => panic!("the overlay closed: {other:?}"),
    }
}

/// The dialog's buttons are the one thing in lazysync that looks like
/// something to click, so they are -- but answering still takes two clicks,
/// because every question it asks guards something irreversible.
#[test]
fn clicking_a_dialog_button_selects_it_and_a_double_click_answers() {
    let mut h = Harness::new();
    h.app.overlay = Overlay::Question(Question::new(
        "Close the connection?",
        Pending::Nothing,
        Pending::Nothing,
    ));
    h.draw();

    h.click("yes");

    match &h.app.overlay {
        Overlay::Question(question) => assert_eq!(question.answer, Answer::Yes),
        other => panic!("one click answered the question: {other:?}"),
    }

    h.draw();
    h.double_click("yes");
    assert!(
        matches!(h.app.overlay, Overlay::None),
        "the second click should have taken the answer"
    );
}

// ── The connection list ──────────────────────────────────────────────────

/// A connection that needs no network to be listed.
fn named(name: &str) -> Bookmark {
    Bookmark {
        name: name.to_string(),
        ..Bookmark::local_transfer()
    }
}

#[test]
fn clicking_a_connection_selects_it() {
    let mut h = Harness::new();
    h.app.screen = Screen::Bookmarks;
    h.app.store = BookmarkStore {
        bookmarks: vec![named("alpha-host"), named("beta-host")],
        ..BookmarkStore::default()
    };
    h.draw();

    h.click("beta-host");
    assert_eq!(h.app.bookmark_cursor, 1);

    h.draw();
    h.click("alpha-host");
    assert_eq!(h.app.bookmark_cursor, 0);
}

// ── The connection form ──────────────────────────────────────────────────

#[test]
fn clicking_a_form_field_focuses_it() {
    let mut h = Harness::new();
    h.app.handle(Action::ShowConnections);
    h.app.handle(Action::NewItem);
    h.draw();

    assert_eq!(h.app.screen, Screen::Form);

    let form = h.app.form.clone().expect("a form");
    let (index, field) = form
        .fields
        .iter()
        .enumerate()
        .find(|(_, field)| form.is_relevant(field.id) && field.label != form.fields[0].label)
        .expect("a second relevant field");
    let label = field.label.to_string();

    h.click(&label);
    assert_eq!(h.app.form.as_ref().unwrap().cursor, index);
}

/// A dimmed field is not reachable with the arrow keys either -- it belongs to
/// an auth method that is not selected -- so a click must not be the one way
/// to land on it and type into a box that will be ignored.
#[test]
fn clicking_a_dimmed_field_is_ignored() {
    let mut h = Harness::new();
    h.app.handle(Action::ShowConnections);
    h.app.handle(Action::NewItem);
    h.draw();

    let form = h.app.form.clone().expect("a form");
    // There is always one: the identity file belongs to key auth and the
    // remembered password to password auth, and only one of those is chosen.
    let field = form
        .fields
        .iter()
        .find(|field| !form.is_relevant(field.id))
        .expect("a dimmed field");

    let before = h.app.form.as_ref().unwrap().cursor;
    let label = field.label.to_string();
    h.click(&label);

    assert_eq!(
        h.app.form.as_ref().unwrap().cursor,
        before,
        "{label} is dimmed, so clicking it must do nothing"
    );
}

// ── The confirmation window ──────────────────────────────────────────────

/// Sections were the part of this screen with no visible affordance at all:
/// the heading said "zo to open" and nothing else suggested it was a control.
#[test]
fn double_clicking_a_section_heading_folds_it() {
    let mut h = Harness::new();
    h.write(&h.left().join("fresh.txt"), "new");
    h.reload();

    h.app.handle(Action::SyncPush);
    assert_eq!(h.app.screen, Screen::Confirm);
    h.draw();

    assert!(
        h.screen().contains("fresh.txt"),
        "the row should be listed before the fold:\n{}",
        h.screen()
    );

    h.double_click("Create");

    assert!(h.app.folded.contains(&Section::Create));
    h.draw();
    assert!(
        !h.screen().contains("fresh.txt"),
        "the folded section should have taken its rows with it:\n{}",
        h.screen()
    );
}

/// A single click on a row selects it and leaves the plan alone. Including or
/// excluding a file from a sync by accident is exactly the kind of thing this
/// screen exists to prevent.
#[test]
fn a_single_click_on_a_plan_row_does_not_change_what_will_be_synced() {
    let mut h = Harness::new();
    h.write(&h.left().join("fresh.txt"), "new");
    h.reload();

    h.app.handle(Action::SyncPush);
    h.draw();

    let included = |h: &Harness| {
        h.app
            .plan
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .filter(|row| row.included)
            .count()
    };
    let before = included(&h);

    h.click("fresh.txt");
    assert_eq!(included(&h), before);
    assert!(
        matches!(h.target_on("fresh.txt"), Target::PlanEntry(_)),
        "the row is what was clicked"
    );

    h.double_click("fresh.txt");
    assert_eq!(
        included(&h),
        before - 1,
        "the second click excludes it, as space does"
    );
}

// ── The audit log ────────────────────────────────────────────────────────

#[test]
fn clicking_a_logged_run_selects_it() {
    let mut h = Harness::new();
    h.app.audit_entries = vec![
        lazysync::sync::audit::Entry::refusal(
            chrono::Utc::now(),
            "mirror push",
            "a",
            "b",
            "too many deletions",
        ),
        lazysync::sync::audit::Entry::refusal(
            chrono::Utc::now(),
            "push",
            "a",
            "b",
            "the second run",
        ),
    ];
    h.app.screen = Screen::Log;
    h.draw();

    h.click("the second run");
    assert_eq!(h.app.log_cursor, 1);

    h.draw();
    h.click("too many deletions");
    assert_eq!(h.app.log_cursor, 0, "and back to the first");
}
