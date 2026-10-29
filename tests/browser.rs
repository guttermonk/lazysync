//! End-to-end tests over the real keymap.
//!
//! These drive [`App`] the way the event loop does -- a key press goes through
//! [`Dispatcher`] against the default keymap and comes out as an action -- so
//! they cover the wiring between keys, modes and pane state without needing a
//! terminal. A unit test that calls `app.handle(Action::EnterDir)` directly
//! would pass even if `l` were bound to nothing.

use std::{fs, path::Path, time::Duration};

use lazysync::{
    app::{App, Overlay},
    config::{Config, OnMissing},
    event::{Dispatch, Dispatcher},
    keys::{Key, ModifiedKey},
};
use tempfile::TempDir;

/// Two deliberately asymmetric trees -- the case this design exists for:
///
/// ```text
/// left/                   right/
///   src/                    src/
///     api/                    api/
///                             web/        <- right only
///   notes/                  docs/         <- right only
///   README.md               deploy.sh
/// ```
fn trees() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        "left/src/api",
        "left/notes",
        "right/src/api",
        "right/src/web",
        "right/docs",
    ] {
        fs::create_dir_all(dir.path().join(path)).unwrap();
    }
    fs::write(dir.path().join("left/README.md"), b"hello").unwrap();
    fs::write(dir.path().join("right/deploy.sh"), b"#!/bin/sh\n").unwrap();
    dir
}

struct Harness {
    app: App,
    dispatcher: Dispatcher,
}

impl Harness {
    fn new(dir: &TempDir, config: Config) -> Self {
        let app = App::new(config, dir.path().join("left"), dir.path().join("right")).unwrap();
        Self {
            dispatcher: Dispatcher::new(Duration::from_millis(500)),
            app,
        }
    }

    /// Type a string of plain keys, one at a time.
    ///
    /// Chords work because the dispatcher accumulates: `press("gg")` resolves
    /// to `MoveTop` on the second key, exactly as it would interactively.
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

    fn tab(&mut self) {
        self.key(ModifiedKey::plain(Key::Tab));
    }

    /// Move the cursor onto a named entry in the focused pane.
    fn select(&mut self, name: &str) {
        let index = self
            .app
            .focused()
            .entries()
            .iter()
            .position(|e| e.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "{name} not in {:?}",
                    self.app
                        .focused()
                        .entries()
                        .iter()
                        .map(|e| &e.name)
                        .collect::<Vec<_>>()
                )
            });
        self.app.handle(lazysync::keys::Action::MoveTop);
        for _ in 0..index {
            self.press("j");
        }
    }

    fn left(&self) -> &Path {
        self.app.panes[0].path()
    }

    fn right(&self) -> &Path {
        self.app.panes[1].path()
    }
}

#[test]
fn panes_open_on_the_two_given_directories() {
    let dir = trees();
    let h = Harness::new(&dir, Config::default());

    assert!(h.left().ends_with("left"));
    assert!(h.right().ends_with("right"));
    // Dotfiles hidden, directories first.
    let names: Vec<_> = h.app.panes[0]
        .entries()
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(names, ["notes", "src", "README.md"]);
}

#[test]
fn vim_keys_navigate_the_focused_pane() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("j");
    assert_eq!(h.app.focused().selected().unwrap().name, "src");

    h.press("l"); // enter
    assert!(h.left().ends_with("left/src"));

    h.press("h"); // back out
    assert!(h.left().ends_with("left"));
    assert_eq!(
        h.app.focused().selected().unwrap().name,
        "src",
        "going up should land on the directory we came out of"
    );
}

#[test]
fn gg_and_shift_g_jump_to_the_ends() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("G");
    assert_eq!(h.app.focused().selected().unwrap().name, "README.md");
    h.press("gg");
    assert_eq!(h.app.focused().selected().unwrap().name, "notes");
}

#[test]
fn tab_switches_panes_and_navigation_follows_focus() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.tab();
    assert_eq!(h.app.focus, 1);

    h.select("docs");
    h.press("l");

    assert!(h.right().ends_with("right/docs"));
    assert!(
        h.left().ends_with("left"),
        "with synchronized browsing off, the other pane must not move"
    );
}

#[test]
fn capital_s_turns_on_synchronized_browsing_and_both_panes_move() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("S");
    assert!(h.app.sync_browse);

    h.select("src");
    h.press("l");

    assert!(h.left().ends_with("left/src"));
    assert!(
        h.right().ends_with("right/src"),
        "the follower should match the relative path, not the absolute one"
    );
}

/// The asymmetric case. Entering a directory the other side lacks must ghost
/// rather than desync, and backing out must re-couple with no intervention.
#[test]
fn a_missing_counterpart_ghosts_and_then_recouples() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("S");
    h.tab(); // focus the remote pane, which has the extra directories
    h.select("docs");
    h.press("l");

    assert!(h.right().ends_with("right/docs"));
    assert!(
        h.app.panes[0].is_ghost(),
        "the local pane should be a ghost"
    );
    assert!(
        h.left().ends_with("left/docs"),
        "a ghost keeps the virtual path: {:?}",
        h.left()
    );
    assert!(h.app.panes[0].is_empty());

    h.press("h"); // back out

    assert!(!h.app.panes[0].is_ghost(), "the panes should re-couple");
    assert!(h.left().ends_with("left"));
    assert!(h.right().ends_with("right"));
}

/// Two levels deep into a tree the other side does not have, then back.
#[test]
fn nested_ghosts_unwind_one_level_at_a_time() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("S");
    h.tab();
    h.select("src");
    h.press("l");
    assert!(!h.app.panes[0].is_ghost());

    h.select("web"); // right only
    h.press("l");
    assert!(h.app.panes[0].is_ghost());
    assert!(h.left().ends_with("left/src/web"));

    h.press("h");
    assert!(!h.app.panes[0].is_ghost());
    assert!(h.left().ends_with("left/src"));
}

#[test]
fn on_missing_stay_holds_position_and_flags_a_desync() {
    let dir = trees();
    let mut config = Config::default();
    config.sync_browse.on_missing = OnMissing::Stay;

    let mut h = Harness::new(&dir, config);
    h.press("S");
    h.tab();
    h.select("docs");
    h.press("l");

    assert!(h.left().ends_with("left"), "the follower should not move");
    assert!(h.app.desynced);
    assert!(!h.app.panes[0].is_ghost());
}

#[test]
fn on_missing_create_makes_the_directory() {
    let dir = trees();
    let mut config = Config::default();
    config.sync_browse.on_missing = OnMissing::Create;

    let mut h = Harness::new(&dir, config);
    h.press("S");
    h.tab();
    h.select("docs");
    h.press("l");

    assert!(h.left().ends_with("left/docs"));
    assert!(!h.app.panes[0].is_ghost());
    assert!(dir.path().join("left/docs").is_dir());
}

/// `ask` must put a question on screen, and answering it must act -- including
/// "no", which ghosts rather than leaving a silent desync.
#[test]
fn on_missing_ask_prompts_and_both_answers_act() {
    let dir = trees();
    let mut config = Config::default();
    config.sync_browse.on_missing = OnMissing::Ask;

    let mut h = Harness::new(&dir, config.clone());
    h.press("S");
    h.tab();
    h.select("docs");
    h.press("l");

    assert!(
        matches!(h.app.overlay, Overlay::Question(_)),
        "expected a question, got {:?}",
        h.app.overlay
    );
    // The question borrows the confirm keymap, so `y` applies.
    h.press("y");
    assert_eq!(h.app.overlay, Overlay::None);
    assert!(dir.path().join("left/docs").is_dir());

    // And again, declining this time.
    let dir2 = trees();
    let mut h2 = Harness::new(&dir2, config);
    h2.press("S");
    h2.tab();
    h2.select("docs");
    h2.press("l");
    h2.press("n");

    assert_eq!(h2.app.overlay, Overlay::None);
    assert!(
        !dir2.path().join("left/docs").exists(),
        "declining must not create it"
    );
    assert!(
        h2.app.panes[0].is_ghost(),
        "declining should still leave the panes coupled, by ghosting"
    );
}

/// Union view is the other half of the asymmetric answer: a remote-only
/// directory has to be selectable from either pane.
#[test]
fn union_view_exposes_one_sided_entries_and_entering_one_ghosts_the_other_pane() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("u");
    assert!(h.app.union_view);
    assert!(
        h.app.sync_browse,
        "union rows only mean something against a shared anchor"
    );

    let rows = h.app.union_rows();
    let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
    // Both sides' entries, merged.
    for expected in ["notes", "src", "docs", "README.md", "deploy.sh"] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }

    // `docs` exists only on the right; entering it moves the right pane and
    // ghosts the left.
    let docs = rows.iter().position(|r| r.name == "docs").unwrap();
    h.app.union_cursor = docs;
    h.press("l");

    assert!(h.right().ends_with("right/docs"));
    assert!(h.left().ends_with("left/docs"));
    assert!(h.app.panes[0].is_ghost());
}

#[test]
fn search_is_incremental_and_slash_n_repeats_it() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("/");
    assert!(matches!(h.app.overlay, Overlay::Prompt(_)));

    // Typed characters are text here, not commands -- `d` must not delete.
    h.press("read");
    assert_eq!(
        h.app.focused().selected().unwrap().name,
        "README.md",
        "search should move the cursor as you type"
    );

    h.key(ModifiedKey::plain(Key::Enter));
    assert_eq!(h.app.overlay, Overlay::None);

    h.press("n"); // repeat; only one match, so it wraps back to itself
    assert_eq!(h.app.focused().selected().unwrap().name, "README.md");
}

/// A prompt must swallow printable keys. If `q` quit while searching, typing
/// a filename would end the program.
#[test]
fn keys_that_are_commands_in_the_browser_are_text_in_a_prompt() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("/");
    h.press("q");
    h.press("S");

    assert!(!h.app.should_quit, "q must not quit while a prompt is open");
    assert!(
        !h.app.sync_browse,
        "S must not toggle while a prompt is open"
    );
    match &h.app.overlay {
        Overlay::Prompt(prompt) => assert_eq!(prompt.input, "qS"),
        other => panic!("expected the prompt to still be open, got {other:?}"),
    }
}

#[test]
fn escape_closes_the_prompt_without_quitting() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("/");
    h.key(ModifiedKey::plain(Key::Esc));

    assert_eq!(h.app.overlay, Overlay::None);
    assert!(!h.app.should_quit);
}

#[test]
fn the_help_overlay_opens_and_closes() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("?");
    assert!(matches!(h.app.overlay, Overlay::Help { .. }));

    // In help mode, `j` scrolls rather than moving a pane cursor.
    h.press("j");
    match h.app.overlay {
        Overlay::Help { scroll } => assert_eq!(scroll, 1),
        ref other => panic!("expected help, got {other:?}"),
    }

    h.press("?"); // the same key closes it
    assert_eq!(h.app.overlay, Overlay::None);
}

#[test]
fn dotfiles_toggle_on_both_panes_together() {
    let dir = trees();
    fs::write(dir.path().join("left/.env"), b"").unwrap();
    fs::write(dir.path().join("right/.env"), b"").unwrap();

    let mut h = Harness::new(&dir, Config::default());
    assert!(!h.app.panes[0].show_hidden);

    h.press(".");

    assert!(
        h.app.panes[0].show_hidden && h.app.panes[1].show_hidden,
        "filtering one pane differently from the other would make the comparison misleading"
    );
    assert!(h.app.panes[1].entries().iter().any(|e| e.name == ".env"));
}

#[test]
fn marks_accumulate_and_clear() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.key(ModifiedKey::plain(Key::Space));
    h.key(ModifiedKey::plain(Key::Space));
    assert_eq!(h.app.focused().marks().len(), 2);

    h.press("mc");
    assert!(h.app.focused().marks().is_empty());

    h.press("ma");
    assert_eq!(h.app.focused().marks().len(), 3);
}

#[test]
fn q_and_zz_both_quit() {
    let dir = trees();

    let mut h = Harness::new(&dir, Config::default());
    h.press("q");
    assert!(h.app.should_quit);

    let mut h = Harness::new(&dir, Config::default());
    h.press("ZZ");
    assert!(h.app.should_quit);
}

/// A rebinding in config has to take effect, including displacing a default
/// chord that the new binding is a prefix of.
#[test]
fn config_keybindings_override_the_defaults() {
    let dir = trees();
    let config: Config = toml::from_str(
        r#"
        [keys.browser]
        extra = [
            ["Quit", "x"],
            ["GoHome", "g"],
        ]
        "#,
    )
    .unwrap();

    let mut h = Harness::new(&dir, config);

    h.press("x");
    assert!(h.app.should_quit, "the rebound key should quit");

    // `g` now acts immediately rather than waiting for the displaced `gg`.
    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [keys.browser]
            extra = [["GoHome", "g"]]
            "#,
        )
        .unwrap()
    });
    h.press("g");
    assert_eq!(
        h.app.focused().path(),
        dirs::home_dir().unwrap(),
        "rebinding g must displace the default gg, or it could only fire after a timeout"
    );
}

// ── Synchronized scrolling ───────────────────────────────────────────────

/// A tree pair with enough entries on each side to scroll, and deliberately
/// different lengths: twelve files on the left, four on the right.
fn tall_trees() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("left")).unwrap();
    fs::create_dir_all(dir.path().join("right")).unwrap();

    for index in 0..12 {
        fs::write(dir.path().join(format!("left/file-{index:02}.txt")), b"x").unwrap();
    }
    for index in 0..4 {
        fs::write(dir.path().join(format!("right/file-{index:02}.txt")), b"x").unwrap();
    }
    dir
}

/// The request: synchronized browsing followed the directory but not the
/// cursor, so scrolling one pane left the other sitting at the top.
#[test]
fn the_follower_tracks_the_leaders_cursor_within_a_directory() {
    let dir = tall_trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("S");
    assert_eq!(h.app.panes[1].cursor(), 0);

    h.press("jjj");

    assert_eq!(h.app.panes[0].cursor(), 3);
    assert_eq!(
        h.app.panes[1].cursor(),
        3,
        "the follower should be on the same row, not still at the top"
    );
}

/// And the viewport with it, which is the half the report was about: the
/// same rows should be on screen in both panes.
#[test]
fn the_follower_tracks_the_leaders_scroll_offset() {
    let dir = tall_trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("S");
    h.app.panes[0].scroll = 6;
    h.press("j");

    assert_eq!(
        h.app.panes[1].scroll,
        h.app.panes[0].scroll.min(h.app.panes[1].len() - 1),
        "the follower's viewport should track the leader's"
    );
}

/// A shorter directory stops at its own last entry rather than being
/// scrolled into blank space below it.
#[test]
fn a_shorter_follower_clamps_to_its_own_last_entry() {
    let dir = tall_trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("S");
    h.app.handle(lazysync::keys::Action::MoveBottom);

    assert_eq!(h.app.panes[0].cursor(), 11, "twelve files on the left");
    assert_eq!(
        h.app.panes[1].cursor(),
        3,
        "four on the right, so it stops on the last one"
    );
}

/// The same rule navigation already follows: only the leader propagates.
/// With `leader = "local"`, scrolling the remote pane is a deliberate look
/// at the other side and must not drag the local one.
#[test]
fn only_the_leaders_cursor_propagates() {
    let dir = tall_trees();
    let mut config = Config::default();
    config.sync_browse.leader = lazysync::config::Leader::Local;

    let mut h = Harness::new(&dir, config);
    h.press("S");
    h.tab();
    h.press("jj");

    assert_eq!(h.app.panes[1].cursor(), 2, "the remote pane moved");
    assert_eq!(
        h.app.panes[0].cursor(),
        0,
        "the local pane leads, so it must not have been dragged"
    );
}

/// Switched off, the panes are independent again -- including their cursors.
#[test]
fn without_synchronized_browsing_the_cursors_are_independent() {
    let dir = tall_trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("jjj");

    assert_eq!(h.app.panes[0].cursor(), 3);
    assert_eq!(h.app.panes[1].cursor(), 0, "nothing should have followed");
}

// ── Opening a file externally ────────────────────────────────────────────

/// Both keys reach the same action. `shift+enter` is the asked-for one;
/// `gx` exists because most terminals cannot send it.
#[test]
fn shift_enter_and_gx_both_open_externally() {
    use lazysync::keys::{Action, Keymaps, Mode, Mods};

    let maps = Keymaps::default();
    let combos = maps.get(Mode::Browser).combos_for(Action::OpenExternal);
    let spellings: Vec<String> = combos.iter().map(ToString::to_string).collect();

    assert!(
        spellings.iter().any(|s| s.contains("enter")),
        "shift+enter should be bound: {spellings:?}"
    );
    assert!(
        spellings.iter().any(|s| s == "gx"),
        "a key that works on every terminal is required too: {spellings:?}"
    );

    // And shift+enter must not have displaced plain Enter, which still
    // enters a directory.
    let mut dispatcher = Dispatcher::new(Duration::from_millis(500));
    assert_eq!(
        dispatcher.key(Mode::Browser, &maps, ModifiedKey::plain(Key::Enter)),
        Dispatch::Act(Action::EnterDir)
    );

    let mut dispatcher = Dispatcher::new(Duration::from_millis(500));
    assert_eq!(
        dispatcher.key(
            Mode::Browser,
            &maps,
            ModifiedKey::new(Key::Enter, Mods::shift())
        ),
        Dispatch::Act(Action::OpenExternal)
    );
}

// No test drives `OpenExternal` to completion on a local file, and that is
// deliberate: the last thing it does is hand the path to `xdg-open`, so a
// test of the happy path would launch whatever the developer's desktop
// opens a `.md` with, on every `cargo test`. The guard before the spawn is
// covered by the remote case in `tests/sftp.rs`, where there is a real
// backend to be refused.

// ── File operations ──────────────────────────────────────────────────────

use lazysync::app::{Answer, Pending};

/// Answer the yes/no dialog with "yes".
fn confirm(h: &mut Harness) {
    match &h.app.overlay {
        Overlay::Question(_) => {}
        other => panic!("expected a confirmation, got {other:?}"),
    }
    h.app.handle(lazysync::keys::Action::Apply);
}

fn names(h: &Harness) -> Vec<String> {
    h.app
        .focused()
        .entries()
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

/// Type a string into an open prompt, then submit.
fn type_and_submit(h: &mut Harness, text: &str) {
    for ch in text.chars() {
        h.app.handle_text(ch);
    }
    h.app.handle(lazysync::keys::Action::Submit);
}

/// Clear whatever a prompt opened pre-filled with.
fn clear_prompt(h: &mut Harness) {
    for _ in 0..64 {
        h.app.handle_backspace();
    }
}

#[test]
fn a_creates_an_empty_file() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("a");
    type_and_submit(&mut h, "notes.txt");

    assert!(dir.path().join("left/notes.txt").is_file());
    assert!(names(&h).contains(&"notes.txt".to_string()));
    assert_eq!(
        h.app.focused().selected().unwrap().name,
        "notes.txt",
        "the thing you just made should be what is selected"
    );
}

#[test]
fn shift_a_creates_a_directory() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("A");
    type_and_submit(&mut h, "build");

    assert!(dir.path().join("left/build").is_dir());
    assert_eq!(h.app.focused().selected().unwrap().name, "build");
}

#[test]
fn r_renames_the_selected_entry() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.select("README.md");
    h.press("r");

    // The prompt opens pre-filled with the current name.
    match &h.app.overlay {
        Overlay::Prompt(p) => assert_eq!(p.input, "README.md"),
        other => panic!("expected a prompt, got {other:?}"),
    }

    clear_prompt(&mut h);
    type_and_submit(&mut h, "GUIDE.md");

    assert!(!dir.path().join("left/README.md").exists());
    assert!(dir.path().join("left/GUIDE.md").is_file());
    assert_eq!(h.app.focused().selected().unwrap().name, "GUIDE.md");
}

/// POSIX rename replaces silently. A file manager that loses a file because
/// you typed a name already in use is not one anybody trusts twice.
#[test]
fn renaming_onto_an_existing_name_is_refused() {
    let dir = trees();
    fs::write(dir.path().join("left/other.md"), b"keep me").unwrap();
    let mut h = Harness::new(&dir, Config::default());
    h.app.focused_mut().reload();

    h.select("README.md");
    h.press("r");
    clear_prompt(&mut h);
    type_and_submit(&mut h, "other.md");

    assert_eq!(
        fs::read_to_string(dir.path().join("left/other.md")).unwrap(),
        "keep me",
        "the existing file must survive"
    );
    assert!(dir.path().join("left/README.md").exists());
    assert!(h.app.status.contains("already exists"), "{}", h.app.status);
}

/// The prompt says *name*, so a path is refused rather than quietly creating
/// something one directory up.
#[test]
fn a_name_with_a_separator_is_refused() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("a");
    type_and_submit(&mut h, "../escaped.txt");

    assert!(!dir.path().join("escaped.txt").exists());
    assert!(h.app.status.contains("no slashes"), "{}", h.app.status);
}

#[test]
fn creating_a_name_that_is_taken_is_refused() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.press("a");
    type_and_submit(&mut h, "README.md");

    assert_eq!(
        fs::read_to_string(dir.path().join("left/README.md")).unwrap(),
        "hello",
        "the existing file must not be truncated"
    );
    assert!(h.app.status.contains("already exists"), "{}", h.app.status);
}

/// Delete asks first, and does nothing until it is answered.
#[test]
fn dd_asks_before_deleting_and_declining_keeps_the_file() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.select("README.md");
    h.press("dd");

    match &h.app.overlay {
        Overlay::Question(q) => {
            assert!(q.text.contains("README.md"), "{:?}", q.text);
            assert!(q.text.contains("cannot be undone"), "{:?}", q.text);
            assert_eq!(q.answer, Answer::No, "the dialog opens on the safe answer");
            assert!(matches!(q.on_yes, Pending::DeleteEntries { .. }));
        }
        other => panic!("expected a confirmation, got {other:?}"),
    }

    assert!(dir.path().join("left/README.md").exists(), "nothing yet");

    h.app.handle(lazysync::keys::Action::Cancel);
    assert!(dir.path().join("left/README.md").exists(), "declined");
}

#[test]
fn confirming_a_delete_removes_the_file() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.select("README.md");
    h.press("dd");
    confirm(&mut h);

    assert!(!dir.path().join("left/README.md").exists());
    assert!(!names(&h).contains(&"README.md".to_string()));
}

/// Marks win over the cursor, as they do for Enter.
#[test]
fn delete_takes_the_marked_entries_when_there_are_any() {
    let dir = trees();
    fs::write(dir.path().join("left/a.txt"), b"a").unwrap();
    fs::write(dir.path().join("left/b.txt"), b"b").unwrap();
    let mut h = Harness::new(&dir, Config::default());
    h.app.focused_mut().reload();

    h.select("a.txt");
    h.key(ModifiedKey::plain(Key::Space));
    h.select("b.txt");
    h.key(ModifiedKey::plain(Key::Space));

    // Cursor is elsewhere; the marks are what should go.
    h.select("README.md");
    h.press("dd");
    confirm(&mut h);

    assert!(!dir.path().join("left/a.txt").exists());
    assert!(!dir.path().join("left/b.txt").exists());
    assert!(
        dir.path().join("left/README.md").exists(),
        "the unmarked row under the cursor must survive"
    );
}

/// Deleting a directory is recursive, and the dialog has to say so — the
/// keystroke is the same one that removes a single file.
#[test]
fn deleting_a_directory_says_it_takes_the_contents() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.select("src");
    h.press("dd");

    match &h.app.overlay {
        Overlay::Question(q) => assert!(
            q.text.contains("everything in it"),
            "recursion must be named: {:?}",
            q.text
        ),
        other => panic!("expected a confirmation, got {other:?}"),
    }

    confirm(&mut h);
    assert!(!dir.path().join("left/src").exists());
    assert!(!dir.path().join("left/src/api").exists());
}

#[cfg(unix)]
#[test]
fn p_changes_the_permission_bits() {
    use std::os::unix::fs::PermissionsExt;

    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.select("README.md");
    h.press("p");

    // Pre-filled with what it has now, so the usual edit is one digit.
    match &h.app.overlay {
        Overlay::Prompt(p) => assert_eq!(p.input.len(), 3, "pre-filled octal: {:?}", p.input),
        other => panic!("expected a prompt, got {other:?}"),
    }

    clear_prompt(&mut h);
    type_and_submit(&mut h, "600");

    let mode = fs::metadata(dir.path().join("left/README.md"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_mode_that_is_not_octal_is_refused() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.select("README.md");
    h.press("p");
    clear_prompt(&mut h);
    type_and_submit(&mut h, "rwx");

    assert!(h.app.status.contains("octal"), "{}", h.app.status);
}

#[test]
fn a_mode_out_of_range_is_refused() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.select("README.md");
    h.press("p");
    clear_prompt(&mut h);
    type_and_submit(&mut h, "77777");

    assert!(h.app.status.contains("out of range"), "{}", h.app.status);
}

/// Every file-op key is bound, or the feature is unreachable whatever the
/// handlers do.
#[test]
fn the_file_operations_are_all_bound_in_the_browser() {
    use lazysync::keys::{Action, Keymaps, Mode};

    let maps = Keymaps::default();
    let keymap = maps.get(Mode::Browser);

    for (action, expected) in [
        (Action::NewFile, "a"),
        (Action::NewDir, "A"),
        (Action::Rename, "r"),
        (Action::DeleteFile, "dd"),
        (Action::Chmod, "p"),
    ] {
        let combos: Vec<String> = keymap
            .combos_for(action)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            combos.contains(&expected.to_string()),
            "{action} should be on {expected:?}, got {combos:?}"
        );
    }
}

// ── Editing ──────────────────────────────────────────────────────────────

/// A local file is edited where it lies, not copied to scratch and back —
/// copying would turn an edit into a replace, and anything watching the
/// file would see something that did not happen.
#[test]
fn e_opens_a_local_file_in_place() {
    let dir = trees();
    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["true"]
            "#,
        )
        .unwrap()
    });

    h.select("README.md");
    h.press("e");

    let request = h.app.edit_request.as_ref().expect("an editor request");
    assert_eq!(
        request.local,
        dir.path().join("left/README.md"),
        "the editor should get the real path, not a copy"
    );
    assert!(request.origin.is_none(), "nothing to upload afterwards");
    assert_eq!(
        request.command,
        vec!["true", request.local.to_str().unwrap()]
    );
}

/// A directory is not a file, and the refusal happens before the terminal
/// is handed over rather than after.
#[test]
fn e_on_a_directory_is_refused() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    h.select("src");
    h.press("e");

    assert!(h.app.edit_request.is_none(), "nothing should be launched");
    assert!(h.app.status.contains("is a directory"), "{}", h.app.status);
}

/// With no editor anywhere, say which three places one can come from.
#[test]
fn e_without_an_editor_says_where_to_set_one() {
    let dir = trees();
    let mut h = Harness::new(&dir, Config::default());

    // The harness inherits the developer's environment, so this only
    // asserts the message when there genuinely is no editor set.
    if std::env::var_os("VISUAL").is_some() || std::env::var_os("EDITOR").is_some() {
        return;
    }

    h.select("README.md");
    h.press("e");

    assert!(h.app.edit_request.is_none());
    assert!(h.app.status.contains("$EDITOR"), "{}", h.app.status);
}

/// The size guard fires before any transfer, because the point of it is to
/// not spend the transfer.
#[test]
fn a_file_over_the_editor_limit_is_refused() {
    let dir = trees();
    fs::write(dir.path().join("left/big.bin"), vec![0u8; 4096]).unwrap();

    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["true"]
            max_bytes = 1024
            "#,
        )
        .unwrap()
    });
    h.app.focused_mut().reload();

    h.select("big.bin");
    h.press("e");

    assert!(h.app.edit_request.is_none());
    assert!(h.app.status.contains("max_bytes"), "{}", h.app.status);
}

/// An editor that wrote nothing causes no upload and no new mtime. This is
/// the local case, where "no upload" means the pane simply reloads.
#[test]
fn an_editor_that_changed_nothing_says_so() {
    let dir = trees();
    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["true"]
            "#,
        )
        .unwrap()
    });

    h.select("README.md");
    h.press("e");
    let request = h.app.edit_request.take().expect("a request");

    // Stand in for the event loop: the editor ran and exited.
    h.app.finish_edit(request);

    assert_eq!(h.app.status, "unchanged", "{}", h.app.status);
}

/// And one that did write is reported as a save.
#[test]
fn an_editor_that_wrote_is_reported_as_a_save() {
    let dir = trees();
    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["true"]
            "#,
        )
        .unwrap()
    });

    h.select("README.md");
    h.press("e");
    let request = h.app.edit_request.take().expect("a request");

    // What the editor would have done.
    std::thread::sleep(Duration::from_millis(10));
    fs::write(dir.path().join("left/README.md"), b"edited").unwrap();

    h.app.finish_edit(request);
    assert!(h.app.status.starts_with("saved"), "{}", h.app.status);
}

/// `[editor] command` beats the environment, which is what it is for.
#[test]
fn the_configured_editor_wins_over_the_environment() {
    let dir = trees();
    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["my-editor", "--wait"]
            "#,
        )
        .unwrap()
    });

    h.select("README.md");
    h.press("e");

    let request = h.app.edit_request.as_ref().expect("a request");
    assert_eq!(request.command[0], "my-editor");
    assert_eq!(request.command[1], "--wait");
    assert_eq!(request.command.len(), 3, "the path is appended last");
}

/// A binary file goes to the desktop handler rather than into the editor.
///
/// Asserted through `prepare_edit`'s *decision* rather than by pressing the
/// key: the acting branch ends in `xdg-open`, so a test that drove it would
/// launch whatever the developer's desktop opens a PNG with.
#[test]
fn a_binary_file_is_routed_away_from_the_editor() {
    use lazysync::app::EditPlan;

    let dir = trees();
    // A PNG header: the eight-byte signature starts with a NUL at index 3.
    fs::write(
        dir.path().join("left/shot.png"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR",
    )
    .unwrap();

    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["true"]
            "#,
        )
        .unwrap()
    });
    h.app.focused_mut().reload();

    h.select("shot.png");
    match h.app.plan_edit() {
        Ok(EditPlan::Desktop { name, .. }) => assert_eq!(name, "shot.png"),
        other => panic!("a PNG should not go to the editor: {other:?}"),
    }
}

/// And a text file still does not.
#[test]
fn a_text_file_still_goes_to_the_editor() {
    use lazysync::app::EditPlan;

    let dir = trees();
    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["true"]
            "#,
        )
        .unwrap()
    });

    h.select("README.md");
    match h.app.plan_edit() {
        Ok(EditPlan::Edit(request)) => {
            assert_eq!(request.local, dir.path().join("left/README.md"));
        }
        other => panic!("text belongs in the editor: {other:?}"),
    }
}

/// An empty file is text. There is no NUL in nothing, and the file you just
/// made with `a` should open rather than be handed to an image viewer.
#[test]
fn an_empty_file_counts_as_text() {
    use lazysync::app::EditPlan;

    let dir = trees();
    fs::write(dir.path().join("left/fresh.txt"), b"").unwrap();

    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["true"]
            "#,
        )
        .unwrap()
    });
    h.app.focused_mut().reload();

    h.select("fresh.txt");
    assert!(matches!(h.app.plan_edit(), Ok(EditPlan::Edit(_))));
}

/// `text_only = false` is the escape hatch for an editor you *want*
/// binaries in -- a hex editor, which this would otherwise make unreachable
/// from the key that is meant to run it.
#[test]
fn text_only_false_sends_everything_to_the_editor() {
    use lazysync::app::EditPlan;

    let dir = trees();
    fs::write(dir.path().join("left/shot.png"), b"\x89PNG\r\n\x1a\n\x00").unwrap();

    let mut h = Harness::new(&dir, {
        toml::from_str(
            r#"
            [editor]
            command = ["hexedit"]
            text_only = false
            "#,
        )
        .unwrap()
    });
    h.app.focused_mut().reload();

    h.select("shot.png");
    match h.app.plan_edit() {
        Ok(EditPlan::Edit(request)) => assert_eq!(request.command[0], "hexedit"),
        other => panic!("the opt-out should reach the editor: {other:?}"),
    }
}
