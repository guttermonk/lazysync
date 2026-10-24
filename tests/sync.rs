//! Planning, reviewing and applying a sync, through the real keymap.
//!
//! These drive [`App`] the way the event loop does, so they cover the wiring
//! between keys, the confirmation window and the executor. A unit test that
//! called `app.begin_sync(..)` directly would pass even if `sp` were bound to
//! nothing.
//!
//! The emphasis is on the properties that matter if they are wrong: nothing is
//! transferred before `y`, an excluded row stays excluded, a plan whose
//! destination moved under it is skipped rather than clobbered, and a mirror
//! that would empty the destination is refused.

use std::{fs, path::Path, time::Duration};

use filetime::FileTime;
use lazysync::{
    app::{App, Overlay, Screen},
    config::{Config, Safety},
    event::{Dispatch, Dispatcher},
    keys::{Key, ModifiedKey},
    sync::plan::{Action, Section},
};
use tempfile::TempDir;

/// Write a file with a fixed mtime, so comparisons test the rule rather than
/// how fast the test ran.
fn write(path: &Path, content: &str, epoch: i64) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
    filetime::set_file_mtime(path, FileTime::from_unix_time(epoch, 0)).unwrap();
}

fn mtime_of(path: &Path) -> i64 {
    FileTime::from_last_modification_time(&fs::metadata(path).unwrap()).unix_seconds()
}

struct Harness {
    app: App,
    dispatcher: Dispatcher,
    dir: TempDir,
}

impl Harness {
    fn new(config: Config) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("left")).unwrap();
        fs::create_dir_all(dir.path().join("right")).unwrap();

        let app = App::new(config, dir.path().join("left"), dir.path().join("right")).unwrap();

        Self {
            dispatcher: Dispatcher::new(Duration::from_millis(500)),
            app,
            dir,
        }
    }

    fn left(&self) -> std::path::PathBuf {
        self.dir.path().join("left")
    }

    fn right(&self) -> std::path::PathBuf {
        self.dir.path().join("right")
    }

    /// Re-read both panes, after the fixture has written files.
    fn refresh(&mut self) {
        for pane in &mut self.app.panes {
            pane.reload();
        }
    }

    fn press(&mut self, keys: &str) {
        for ch in keys.chars() {
            self.key(ModifiedKey::plain(Key::Char(ch)));
        }
    }

    fn key(&mut self, key: ModifiedKey) {
        let mode = self.app.mode();
        match self.dispatcher.key(mode, &self.app.keymaps, key) {
            Dispatch::Act(action) => self.app.handle(action),
            Dispatch::Text(ch) => self.app.handle_text(ch),
            Dispatch::Backspace => self.app.handle_backspace(),
            Dispatch::Pending | Dispatch::Unbound => {}
        }
    }

    fn space(&mut self) {
        self.key(ModifiedKey::plain(Key::Space));
    }

    /// Drive the transfer to completion, as the main loop does.
    ///
    /// `y` starts a queue and returns; the loop in `tui::main_loop` steps it a
    /// chunk per frame. These tests stand in for that loop, which is also why
    /// they exercise the stepping rather than a separate blocking path.
    fn drain(&mut self) {
        let mut guard = 0;
        while self.app.transfer_is_active() {
            self.app.step_transfer();
            guard += 1;
            assert!(guard < 200_000, "the transfer did not terminate");
        }
    }

    /// `y`, then run the transfer out.
    ///
    /// Leaves the progress screen up, as the real thing does: it becomes a
    /// report once finished and waits to be acknowledged.
    fn apply(&mut self) {
        self.press("y");
        self.drain();
    }

    /// Dismiss the finished progress screen, back to the browser.
    fn dismiss(&mut self) {
        self.key(ModifiedKey::plain(Key::Esc));
    }

    /// Move the confirmation cursor onto a named row.
    fn select_row(&mut self, name: &str) {
        let plan = self.app.plan.as_ref().expect("a plan should be open");
        let entries = lazysync::ui::confirm::entries(plan, &self.app.folded);

        let position = entries
            .iter()
            .position(|e| match e {
                lazysync::ui::confirm::Entry::Row { row, .. } => row.path == Path::new(name),
                _ => false,
            })
            .unwrap_or_else(|| panic!("no row for {name}"));

        self.app.plan_cursor = position;
    }
}

/// A tree pair with one of each interesting case.
fn fixture(h: &Harness) {
    // Changed: source newer, bigger.
    write(&h.left().join("index.html"), "hello world", 2_000_000);
    write(&h.right().join("index.html"), "hello", 1_000_000);
    // Identical.
    write(&h.left().join("README.md"), "same", 1_000_000);
    write(&h.right().join("README.md"), "same", 1_000_000);
    // Destination newer: the dangerous case.
    write(&h.left().join("config.toml"), "aaaaa", 1_000_000);
    write(&h.right().join("config.toml"), "bbbbb", 3_000_000);
    // New, nested.
    write(&h.left().join("src/main.rs"), "fn main() {}", 2_000_000);
    // Extraneous on the destination.
    write(&h.right().join("obsolete.txt"), "old junk", 1_000_000);
}

#[test]
fn sp_plans_a_push_and_transfers_nothing_until_it_is_confirmed() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");

    assert_eq!(h.app.screen, Screen::Confirm);
    let plan = h.app.plan.as_ref().expect("a plan");

    assert_eq!(plan.count(Section::Create), 1);
    assert_eq!(plan.count(Section::Overwrite), 2);
    assert_eq!(plan.count(Section::Delete), 0, "a push must not delete");
    assert_eq!(plan.count(Section::Skipped), 1);

    // Nothing has moved yet. This is the whole premise.
    assert_eq!(
        fs::read_to_string(h.right().join("index.html")).unwrap(),
        "hello"
    );
    assert!(!h.right().join("src/main.rs").exists());
}

/// Both sides' size and time on every overwrite row -- the thing the window
/// exists to show.
#[test]
fn every_overwrite_row_carries_both_sides() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();
    h.press("sp");

    let plan = h.app.plan.as_ref().unwrap();
    for row in plan.section(Section::Overwrite) {
        let source = row.source.expect("a source side");
        let dest = row.dest.expect("a destination side");
        assert!(
            source.mtime.is_some() && dest.mtime.is_some(),
            "{}: the window cannot show a comparison it was not given",
            row.path.display()
        );
        assert!(source.size > 0 && dest.size > 0);
    }
}

/// The red case: equal sizes, newer destination.
#[test]
fn a_newer_destination_is_flagged_in_the_plan_and_counted() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();
    h.press("sp");

    let plan = h.app.plan.as_ref().unwrap();
    let row = plan
        .rows
        .iter()
        .find(|r| r.path == Path::new("config.toml"))
        .unwrap();

    assert!(row.clobbers_newer());
    assert_eq!(plan.totals().clobbers_newer, 1);
}

#[test]
fn y_applies_the_plan() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");
    h.apply();

    assert!(h.app.transfer.is_some(), "the progress screen should be up");
    assert_eq!(
        fs::read_to_string(h.right().join("index.html")).unwrap(),
        "hello world"
    );
    assert_eq!(
        fs::read_to_string(h.right().join("src/main.rs")).unwrap(),
        "fn main() {}",
        "a nested create should bring its directory with it"
    );
    assert!(
        h.app.status.contains("applied"),
        "the status should report what happened: {}",
        h.app.status
    );
}

/// Timestamps have to survive, or the next sync re-copies everything.
#[test]
fn applying_preserves_timestamps() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");
    h.apply();

    assert_eq!(
        mtime_of(&h.right().join("index.html")),
        mtime_of(&h.left().join("index.html"))
    );

    // And the proof of the pudding: planning again finds nothing to do.
    // The progress screen has to be dismissed first -- it stays up as a
    // report until acknowledged, so the first keypress after a sync closes it
    // rather than starting another one.
    h.dismiss();
    assert_eq!(h.app.screen, Screen::Browser);

    h.refresh();
    h.press("sp");
    assert_eq!(
        h.app.screen,
        Screen::Browser,
        "a second push should find the trees already matching, not re-copy them: {}",
        h.app.status
    );
    assert!(h.app.status.contains("already match"), "{}", h.app.status);
}

#[test]
fn n_cancels_without_transferring_anything() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");
    h.press("n");

    assert_eq!(h.app.screen, Screen::Browser);
    assert!(h.app.plan.is_none());
    assert_eq!(
        fs::read_to_string(h.right().join("index.html")).unwrap(),
        "hello",
        "cancelling must leave the destination untouched"
    );
    assert!(h.app.status.contains("nothing was transferred"));
}

/// Excluding a row with `space` has to actually exclude it.
#[test]
fn an_excluded_row_is_not_applied_and_the_totals_follow() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");
    let before = h.app.plan.as_ref().unwrap().totals().bytes_out;

    h.select_row("config.toml");
    h.space();

    let after = h.app.plan.as_ref().unwrap().totals();
    assert!(
        after.bytes_out < before,
        "the footer must recompute: {before} -> {}",
        after.bytes_out
    );
    assert_eq!(after.clobbers_newer, 0, "the dangerous row was excluded");

    h.apply();

    assert_eq!(
        fs::read_to_string(h.right().join("config.toml")).unwrap(),
        "bbbbb",
        "an excluded row must not be written"
    );
    assert_eq!(
        fs::read_to_string(h.right().join("index.html")).unwrap(),
        "hello world",
        "...while the included ones still are"
    );
}

#[test]
fn o_includes_only_the_selected_row() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");
    h.select_row("index.html");
    h.press("o");

    assert_eq!(h.app.plan.as_ref().unwrap().totals().operations(), 1);

    h.apply();
    assert_eq!(
        fs::read_to_string(h.right().join("index.html")).unwrap(),
        "hello world"
    );
    assert!(
        !h.right().join("src/main.rs").exists(),
        "only the selected row should have been applied"
    );
}

#[test]
fn a_excludes_nothing_and_includes_everything_again() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");
    h.select_row("index.html");
    h.press("o"); // only this one
    assert_eq!(h.app.plan.as_ref().unwrap().totals().operations(), 1);

    h.press("a"); // all again
    assert_eq!(h.app.plan.as_ref().unwrap().totals().operations(), 3);
}

#[test]
fn i_inverts_the_inclusions() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");
    let all = h.app.plan.as_ref().unwrap().totals().operations();

    h.press("i");
    assert_eq!(
        h.app.plan.as_ref().unwrap().totals().operations(),
        0,
        "inverting a fully-included plan should exclude everything"
    );

    h.press("i");
    assert_eq!(h.app.plan.as_ref().unwrap().totals().operations(), all);
}

/// The skipped section starts folded, because a review list that is mostly
/// `=` rows is one nobody reads.
#[test]
fn the_identical_files_start_folded_and_can_be_opened() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();
    h.press("sp");

    assert!(h.app.folded.contains(&Section::Skipped));

    // Move to the Skipped heading and unfold it.
    let plan = h.app.plan.as_ref().unwrap();
    let entries = lazysync::ui::confirm::entries(plan, &h.app.folded);
    let heading = entries
        .iter()
        .position(|e| {
            matches!(
                e,
                lazysync::ui::confirm::Entry::Heading {
                    section: Section::Skipped,
                    ..
                }
            )
        })
        .unwrap();
    h.app.plan_cursor = heading;

    h.press("zo");
    assert!(!h.app.folded.contains(&Section::Skipped));

    h.press("zc");
    assert!(h.app.folded.contains(&Section::Skipped));
}

// ── Mirror and the safety rails ─────────────────────────────────────────────

#[test]
fn sm_plans_a_mirror_with_deletions() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sm");

    let plan = h.app.plan.as_ref().expect("a plan");
    assert_eq!(plan.count(Section::Delete), 1);
    assert_eq!(
        plan.rows
            .iter()
            .find(|r| r.action == Action::Delete)
            .unwrap()
            .path,
        Path::new("obsolete.txt")
    );
}

/// Deletions are recoverable by default, and the window says so.
#[test]
fn a_mirror_moves_deletions_to_the_backup_directory_by_default() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sm");
    h.apply();

    assert!(!h.right().join("obsolete.txt").exists());
    assert_eq!(
        fs::read_to_string(h.right().join(".lazysync-backup/obsolete.txt")).unwrap(),
        "old junk",
        "the default policy has to leave a deletion recoverable"
    );
}

/// With permanent deletion configured, a second confirmation is required --
/// the one irreversible part of a plan deserves to be named on its own.
#[test]
fn permanent_deletion_asks_a_second_time() {
    let config = Config {
        safety: Safety {
            backup_dir: String::new(),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut h = Harness::new(config);
    fixture(&h);
    h.refresh();

    h.press("sm");
    h.press("y");

    assert!(
        matches!(h.app.overlay, Overlay::Question(_)),
        "permanent deletion should ask again: {:?}",
        h.app.overlay
    );
    assert!(
        h.right().join("obsolete.txt").exists(),
        "nothing should have happened yet"
    );

    // Declining leaves the plan open so the deletions can be excluded.
    h.press("n");
    assert_eq!(h.app.screen, Screen::Confirm);
    assert!(h.right().join("obsolete.txt").exists());

    // Accepting goes through.
    h.press("y");
    h.press("y");
    h.drain();
    assert!(!h.right().join("obsolete.txt").exists());
    assert!(!h.right().join(".lazysync-backup").exists());
}

/// The mistyped-root guard, end to end.
#[test]
fn a_mirror_that_would_empty_the_destination_is_refused() {
    let mut h = Harness::new(Config::default());
    // An empty source against a populated destination: the signature of a
    // mirror pointed at the wrong place.
    write(&h.right().join("precious-a"), "a", 1_000_000);
    write(&h.right().join("precious-b"), "b", 1_000_000);
    h.refresh();

    h.press("sm");

    let plan = h.app.plan.as_ref().expect("a plan, with a refusal on it");
    assert!(
        plan.refusal.is_some(),
        "wiping the destination must be refused"
    );
    assert!(!plan.can_apply());

    // And `y` must not get through it.
    h.press("y");
    assert!(h.right().join("precious-a").exists());
    assert!(h.right().join("precious-b").exists());
    assert!(h.app.status.contains("REFUSED"), "{}", h.app.status);
}

/// A refusal has to be escapable by excluding rows, or the window is a dead
/// end.
#[test]
fn excluding_the_deletions_clears_a_refusal() {
    let mut h = Harness::new(Config::default());
    write(&h.right().join("precious"), "a", 1_000_000);
    h.refresh();

    h.press("sm");
    assert!(h.app.plan.as_ref().unwrap().refusal.is_some());

    h.press("i"); // invert: exclude everything
    assert!(
        h.app.plan.as_ref().unwrap().refusal.is_none(),
        "excluding the deletions should clear the refusal"
    );
}

// ── Pull ────────────────────────────────────────────────────────────────────

#[test]
fn sl_plans_a_pull_in_the_other_direction() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("a"), "old", 1_000_000);
    write(&h.right().join("a"), "new content", 2_000_000);
    h.refresh();

    h.press("sl");

    let plan = h.app.plan.as_ref().expect("a plan");
    assert!(plan.source_root.ends_with("right"));
    assert!(plan.dest_root.ends_with("left"));

    h.apply();
    assert_eq!(
        fs::read_to_string(h.left().join("a")).unwrap(),
        "new content",
        "a pull writes to the local side"
    );
}

// ── Edge cases ──────────────────────────────────────────────────────────────

#[test]
fn syncing_identical_trees_says_so_rather_than_opening_an_empty_window() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("a"), "same", 1_000_000);
    write(&h.right().join("a"), "same", 1_000_000);
    h.refresh();

    h.press("sp");

    assert_eq!(h.app.screen, Screen::Browser);
    assert!(h.app.plan.is_none());
    assert!(h.app.status.contains("already match"), "{}", h.app.status);
}

/// A plan is a snapshot. Applying it against a tree that moved must skip the
/// affected row rather than overwrite something never reviewed.
#[test]
fn a_destination_that_changes_after_planning_is_skipped_not_clobbered() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("a"), "source version", 2_000_000);
    write(&h.right().join("a"), "planned against this", 1_000_000);
    h.refresh();

    h.press("sp");

    // Someone else writes to the destination while the window is open.
    write(&h.right().join("a"), "SOMEONE ELSE'S WORK", 3_000_000);

    h.apply();

    assert_eq!(
        fs::read_to_string(h.right().join("a")).unwrap(),
        "SOMEONE ELSE'S WORK",
        "the plan was stale, so the row must be skipped"
    );
    assert!(h.app.status.contains("skipped"), "{}", h.app.status);
}

#[test]
fn a_ghosted_pane_cannot_be_synced() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("a"), "x", 1_000_000);
    h.refresh();

    // Put the right pane on a path that does not exist.
    h.app.panes[1].ghost_at(h.right().join("not-here"));

    h.press("sp");

    assert_eq!(h.app.screen, Screen::Browser);
    assert!(h.app.plan.is_none());
    assert!(h.app.status.contains("does not exist"), "{}", h.app.status);
}

/// Dotfiles follow the browser's own setting, so a sync cannot include files
/// the panes were not showing.
#[test]
fn hidden_files_are_not_synced_unless_they_are_shown() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join(".env"), "SECRET=1", 1_000_000);
    write(&h.left().join("visible"), "x", 1_000_000);
    h.refresh();

    h.press("sp");
    h.apply();

    assert!(h.right().join("visible").exists());
    assert!(
        !h.right().join(".env").exists(),
        "a sync must not transfer what the panes were hiding"
    );
}

#[test]
fn a_sync_leaves_no_partial_files_behind() {
    let mut h = Harness::new(Config::default());
    fixture(&h);
    h.refresh();

    h.press("sp");
    h.apply();

    let leftovers: Vec<_> = walk(&h.right())
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("lazysync-part"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// ── The transfer queue ──────────────────────────────────────────────────────

/// `y` must return at once, not block until the sync finishes.
#[test]
fn applying_shows_the_progress_screen_rather_than_blocking() {
    let mut h = Harness::new(Config::default());
    // Several chunks' worth, so the transfer cannot complete in one step.
    write(
        &h.left().join("big.bin"),
        &"x".repeat(lazysync::sync::transfer::CHUNK * 4),
        1_000_000,
    );
    h.refresh();

    h.press("sp");
    h.press("y");

    assert_eq!(h.app.screen, Screen::Transfer);
    let transfer = h.app.transfer.as_ref().expect("a transfer");
    assert!(
        !transfer.is_finished(),
        "it must not have run to completion"
    );
    assert!(
        !h.right().join("big.bin").exists(),
        "nothing should be in place yet"
    );
}

#[test]
fn progress_advances_as_the_transfer_is_stepped() {
    let mut h = Harness::new(Config::default());
    write(
        &h.left().join("big.bin"),
        &"x".repeat(lazysync::sync::transfer::CHUNK * 4),
        1_000_000,
    );
    h.refresh();

    h.press("sp");
    h.press("y");

    let mut seen = false;
    for _ in 0..40 {
        h.app.step_transfer();
        let transfer = h.app.transfer.as_ref().unwrap();
        let fraction = transfer.fraction();
        if fraction > 0.0 && fraction < 1.0 {
            seen = true;
            break;
        }
    }
    assert!(seen, "the aggregate bar should move part-way through");
}

#[test]
fn p_pauses_and_resumes() {
    let mut h = Harness::new(Config::default());
    write(
        &h.left().join("big.bin"),
        &"x".repeat(lazysync::sync::transfer::CHUNK * 4),
        1_000_000,
    );
    h.refresh();

    h.press("sp");
    h.press("y");
    for _ in 0..2 {
        h.app.step_transfer();
    }

    h.press("p");
    assert_eq!(
        h.app.transfer.as_ref().unwrap().state(),
        lazysync::sync::TransferState::Paused
    );
    assert!(
        !h.app.transfer_is_active(),
        "a paused transfer must not keep the main loop spinning"
    );

    let before = h.app.transfer.as_ref().unwrap().bytes_done();
    h.app.step_transfer();
    assert_eq!(
        h.app.transfer.as_ref().unwrap().bytes_done(),
        before,
        "stepping a paused transfer must do nothing"
    );

    h.press("p");
    assert!(h.app.transfer_is_active());
    h.drain();
    assert!(h.right().join("big.bin").exists());
}

/// Cancelling mid-transfer has to stop promptly and keep what finished.
#[test]
fn esc_cancels_a_transfer_in_flight() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("a-small"), "done", 1_000_000);
    write(
        &h.left().join("b-big.bin"),
        &"x".repeat(lazysync::sync::transfer::CHUNK * 4),
        1_000_000,
    );
    h.refresh();

    h.press("sp");
    h.press("y");
    for _ in 0..4 {
        h.app.step_transfer();
    }

    h.key(ModifiedKey::plain(Key::Esc));
    h.drain();

    assert!(h.app.transfer.as_ref().unwrap().is_finished());
    assert_eq!(
        fs::read_to_string(h.right().join("a-small")).unwrap(),
        "done",
        "a file finished before the cancel stays finished"
    );
    assert!(
        !h.right().join("b-big.bin").exists(),
        "the file in flight must not appear half-written at its real name"
    );
}

/// The screen is a report once finished, so the next key dismisses it rather
/// than doing something else.
#[test]
fn the_finished_screen_waits_to_be_acknowledged() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("a"), "x", 1_000_000);
    h.refresh();

    h.press("sp");
    h.apply();

    assert_eq!(h.app.screen, Screen::Transfer);
    assert!(h.app.status.contains("applied"), "{}", h.app.status);

    h.dismiss();
    assert_eq!(h.app.screen, Screen::Browser);
    assert!(h.app.transfer.is_none());
}

/// A sync has to leave the panes showing what is actually there now.
#[test]
fn the_panes_are_reloaded_when_a_transfer_finishes() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("new.txt"), "x", 1_000_000);
    h.refresh();

    assert!(!h.app.panes[1].entries().iter().any(|e| e.name == "new.txt"));

    h.press("sp");
    h.apply();

    assert!(
        h.app.panes[1].entries().iter().any(|e| e.name == "new.txt"),
        "the destination pane should show the file that was just written"
    );
}

/// A file larger than any sane buffer must transfer without being held in
/// memory. This checks the result; the streaming is what makes it possible.
#[test]
fn a_multi_chunk_file_transfers_byte_for_byte() {
    let mut h = Harness::new(Config::default());
    // A pattern rather than a constant byte, so a chunk-boundary bug shows up
    // as wrong content rather than passing by luck.
    let payload: Vec<u8> = (0..lazysync::sync::transfer::CHUNK * 3 + 777)
        .map(|i| (i % 251) as u8)
        .collect();
    fs::write(h.left().join("blob.bin"), &payload).unwrap();
    filetime::set_file_mtime(
        h.left().join("blob.bin"),
        FileTime::from_unix_time(1_000_000, 0),
    )
    .unwrap();
    h.refresh();

    h.press("sp");
    h.apply();

    assert_eq!(
        fs::read(h.right().join("blob.bin")).unwrap(),
        payload,
        "a chunked copy has to be byte-identical across the boundaries"
    );
}

// ── Profiles and filters ────────────────────────────────────────────────────

/// A profile's exclude rules have to reach the planner. Without this,
/// "advanced syncing" means pushing `target/` to a web server every time.
#[test]
fn a_profiles_exclude_rules_are_applied() {
    let config: Config = toml::from_str(
        r#"
        [profiles.deploy]
        mode = "push"
        exclude = ["target/", "*.log"]
        "#,
    )
    .unwrap();

    let mut h = Harness::new(config);
    write(&h.left().join("src/main.rs"), "code", 1_000_000);
    write(
        &h.left().join("target/debug/huge"),
        "build output",
        1_000_000,
    );
    write(&h.left().join("debug.log"), "noise", 1_000_000);
    h.refresh();

    h.app.active_profile = Some("deploy".into());
    h.press("sp");

    let planned: Vec<String> = h
        .app
        .plan
        .as_ref()
        .expect("a plan")
        .rows
        .iter()
        .map(|r| r.path.display().to_string())
        .collect();

    assert!(planned.contains(&"src/main.rs".to_string()), "{planned:?}");
    assert!(
        !planned.iter().any(|p| p.starts_with("target")),
        "{planned:?}"
    );
    assert!(!planned.contains(&"debug.log".to_string()), "{planned:?}");
}

/// With no profile, nothing is excluded -- a filter that applied by accident
/// would be worse than none.
#[test]
fn without_a_profile_nothing_is_excluded() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("target/x"), "a", 1_000_000);
    h.refresh();

    h.press("sp");

    assert!(h
        .app
        .plan
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .any(|r| r.path.starts_with("target")));
}

/// A pattern lazysync cannot honour stops the plan rather than being skipped:
/// a filter that silently does nothing is how `target/` ends up on a server.
#[test]
fn an_unsupported_exclude_pattern_refuses_to_plan() {
    let config: Config = toml::from_str(
        r#"
        [profiles.broken]
        mode = "push"
        exclude = ["*.[oa]"]
        "#,
    )
    .unwrap();

    let mut h = Harness::new(config);
    write(&h.left().join("a"), "x", 1_000_000);
    h.refresh();

    h.app.active_profile = Some("broken".into());
    h.press("sp");

    assert!(
        h.app.plan.is_none(),
        "it must not plan with a broken filter"
    );
    assert!(
        h.app.status.contains("character class"),
        "and it must say why: {}",
        h.app.status
    );
}

#[test]
fn respect_gitignore_reads_the_sync_roots_own_file() {
    let config: Config = toml::from_str(
        r#"
        [profiles.deploy]
        mode = "push"
        respect_gitignore = true
        "#,
    )
    .unwrap();

    let mut h = Harness::new(config);
    write(
        &h.left().join(".gitignore"),
        "ignored.txt\n# a comment\n",
        1_000_000,
    );
    write(&h.left().join("ignored.txt"), "no", 1_000_000);
    write(&h.left().join("kept.txt"), "yes", 1_000_000);
    h.refresh();

    h.app.active_profile = Some("deploy".into());
    h.press("sp");

    let planned: Vec<String> = h
        .app
        .plan
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .map(|r| r.path.display().to_string())
        .collect();

    assert!(planned.contains(&"kept.txt".to_string()), "{planned:?}");
    assert!(!planned.contains(&"ignored.txt".to_string()), "{planned:?}");
}

/// `ss` uses the profile's own direction, which is the point of having one.
#[test]
fn ss_plans_using_the_profiles_own_mode() {
    let config: Config = toml::from_str(
        r#"
        [profiles.fetch]
        mode = "pull"
        "#,
    )
    .unwrap();

    let mut h = Harness::new(config);
    write(&h.left().join("a"), "old", 1_000_000);
    write(&h.right().join("a"), "newer from remote", 2_000_000);
    h.refresh();

    h.app.active_profile = Some("fetch".into());
    h.press("ss");

    let plan = h.app.plan.as_ref().expect("a plan");
    assert!(
        plan.source_root.ends_with("right"),
        "a pull profile should read from the remote side"
    );

    h.apply();
    assert_eq!(
        fs::read_to_string(h.left().join("a")).unwrap(),
        "newer from remote"
    );
}

#[test]
fn ss_without_a_profile_says_so() {
    let mut h = Harness::new(Config::default());
    write(&h.left().join("a"), "x", 1_000_000);
    h.refresh();

    h.press("ss");

    assert!(h.app.plan.is_none());
    assert!(h.app.status.contains("no sync profile"), "{}", h.app.status);
}

#[test]
fn a_profile_naming_bisync_is_refused_rather_than_run_as_something_else() {
    let config: Config = toml::from_str(
        r#"
        [profiles.both]
        mode = "bisync"
        "#,
    )
    .unwrap();

    let mut h = Harness::new(config);
    write(&h.left().join("a"), "x", 1_000_000);
    h.refresh();

    h.app.active_profile = Some("both".into());
    h.press("ss");

    assert!(h.app.plan.is_none());
    assert!(h.app.status.contains("phase 6"), "{}", h.app.status);
}

fn walk(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
